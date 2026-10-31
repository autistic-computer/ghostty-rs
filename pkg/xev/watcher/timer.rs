//! Port of libxev `src/watcher/timer.zig`.
//!
//! A timer fires a callback after a specified amount of time. A timer can
//! repeat by returning "rearm" in the callback or by rescheduling the
//! start within the callback.

use std::ffi::c_void;
use std::fmt;

use crate::pkg::xev::common::CallbackAction;
use crate::pkg::xev::epoll::{Completion, Loop, OpResult, TimerError, TimerTrigger};
use crate::pkg::xev::posix::CancelError as XevCancelError;

/// Error that could happen while running a timer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunError {
    /// The timer was canceled before it could expire
    Canceled,

    /// Some unexpected error.
    Unexpected,
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for RunError {}

/// Zig: `Timer.CancelError` (= `xev.CancelError`).
pub type CancelError = XevCancelError;

/// The typed callback of [`Timer::run`] / [`Timer::reset`].
pub type RunCallback<U> =
    fn(ud: *mut U, l: &mut Loop, c: &mut Completion, r: Result<(), RunError>) -> CallbackAction;

/// The typed callback of [`Timer::cancel`].
pub type CancelCallback<U> =
    fn(ud: *mut U, l: &mut Loop, c: &mut Completion, r: Result<(), CancelError>) -> CallbackAction;

/// An implementation that uses the loop timer methods.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Timer {}

/// Wrapper converting the raw loop result into the typed run callback.
fn run_callback<U>(
    ud: *mut c_void,
    l: &mut Loop,
    c: &mut Completion,
    r: OpResult,
) -> CallbackAction {
    // SAFETY: `user_callback` was set from a `RunCallback<U>` by `Timer::run`
    // or `Timer::reset`, which also installed this wrapper.
    let cb: RunCallback<U> =
        unsafe { std::mem::transmute::<*const (), RunCallback<U>>(c.user_callback) };
    let r = match r {
        OpResult::Timer(Ok(TimerTrigger::Request | TimerTrigger::Expiration)) => Ok(()),
        OpResult::Timer(Ok(TimerTrigger::Cancel)) => Err(RunError::Canceled),
        OpResult::Timer(Err(TimerError::Unexpected)) => Err(RunError::Unexpected),
        _ => unreachable!("timer callback with non-timer result"),
    };
    cb(ud as *mut U, l, c, r)
}

/// Wrapper converting the raw loop result into the typed cancel callback.
fn cancel_callback<U>(
    ud: *mut c_void,
    l: &mut Loop,
    c: &mut Completion,
    r: OpResult,
) -> CallbackAction {
    // SAFETY: `user_callback` was set from a `CancelCallback<U>` by
    // `Timer::cancel`, which also installed this wrapper.
    let cb: CancelCallback<U> =
        unsafe { std::mem::transmute::<*const (), CancelCallback<U>>(c.user_callback) };
    let r = match r {
        OpResult::Cancel(r) => r,
        _ => unreachable!("cancel callback with non-cancel result"),
    };
    cb(ud as *mut U, l, c, r)
}

impl Timer {
    /// Create a new timer.
    pub fn init() -> Result<Timer, crate::pkg::xev::Error> {
        Ok(Timer {})
    }

    pub fn deinit(&self) {
        // Nothing for now.
    }

    /// Start the timer. The timer will execute in next_ms milliseconds from
    /// now.
    ///
    /// This will use the monotonic clock on your system if available so
    /// this is immune to system clock changes or drift. The callback is
    /// guaranteed to fire NO EARLIER THAN "next_ms" milliseconds. We can't
    /// make any guarantees about exactness or time bounds because its possible
    /// for your OS to just... pause.. the process for an indefinite period of
    /// time.
    ///
    /// Like everything else in libxev, if you want something to repeat, you
    /// must then requeue the completion manually. This punts off one of the
    /// "hard" aspects of timers: it is up to you to determine what the semantic
    /// meaning of intervals are. For example, if you want a timer to repeat every
    /// 10 seconds, is it every 10th second of a wall clock? every 10th second
    /// after an invocation? every 10th second after the work time from the
    /// invocation? You have the power to answer these questions, manually.
    ///
    /// # Safety
    ///
    /// `c` must not move and must stay valid until it is dead again, and
    /// `userdata` must stay valid for the callback (see
    /// [`crate::pkg::xev::epoll`]).
    pub unsafe fn run<U>(
        &self,
        l: &mut Loop,
        c: &mut Completion,
        next_ms: u64,
        userdata: *mut U,
        cb: RunCallback<U>,
    ) {
        // SAFETY: forwarded caller guarantee.
        unsafe { l.timer(c, next_ms, userdata as *mut c_void, run_callback::<U>) };
        c.user_callback = cb as *const ();
    }

    /// Reset a timer to execute in next_ms milliseconds. If the timer
    /// is already started, this will stop it and restart it. If the
    /// timer has never been started, this is equivalent to running "run".
    /// In every case, the timer callback is updated to the given userdata
    /// and callback.
    ///
    /// This requires an additional completion c_cancel to represent
    /// the need to possibly cancel the previous timer. You can check
    /// if c_cancel was used by checking the state() after the call.
    ///
    /// # Safety
    ///
    /// Same as [`Timer::run`], for both `c` and `c_cancel`.
    pub unsafe fn reset<U>(
        &self,
        l: &mut Loop,
        c: &mut Completion,
        c_cancel: &mut Completion,
        next_ms: u64,
        userdata: *mut U,
        cb: RunCallback<U>,
    ) {
        // SAFETY: forwarded caller guarantee.
        unsafe {
            l.timer_reset(
                c,
                c_cancel,
                next_ms,
                userdata as *mut c_void,
                run_callback::<U>,
            )
        };
        c.user_callback = cb as *const ();
    }

    /// Cancel a previously started timer. The timer to cancel used the completion
    /// "c_cancel". A new completion "c" must be specified which will be called
    /// with the callback once cancellation is complete.
    ///
    /// The original timer will still have its callback fired but with the
    /// error "error.Canceled".
    ///
    /// # Safety
    ///
    /// Same as [`Timer::run`], for both `c_timer` and `c_cancel`.
    pub unsafe fn cancel<U>(
        &self,
        l: &mut Loop,
        c_timer: &mut Completion,
        c_cancel: &mut Completion,
        userdata: *mut U,
        cb: CancelCallback<U>,
    ) {
        *c_cancel = Completion::new(
            crate::pkg::xev::epoll::Operation::Cancel { c: c_timer },
            userdata as *mut c_void,
            cancel_callback::<U>,
        );
        c_cancel.user_callback = cb as *const ();

        // SAFETY: forwarded caller guarantee.
        unsafe { l.add(c_cancel) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pkg::xev::common::{CompletionState, Options, RunMode};

    fn set_true(
        ud: *mut bool,
        _: &mut Loop,
        _: &mut Completion,
        r: Result<(), RunError>,
    ) -> CallbackAction {
        r.unwrap();
        // SAFETY: tests pass a pointer to a live bool.
        unsafe { *ud = true };
        CallbackAction::Disarm
    }

    #[test]
    fn timer() {
        let mut l = Loop::init(Options::default()).unwrap();
        let timer = Timer::init().unwrap();

        // Add the timer
        let mut called = false;
        let mut c1 = Completion::default();
        unsafe { timer.run(&mut l, &mut c1, 1, &mut called, set_true) };

        // Wait
        l.run(RunMode::UntilDone).unwrap();
        assert!(called);
        timer.deinit();
    }

    #[test]
    fn timer_reset() {
        let mut l = Loop::init(Options::default()).unwrap();
        let timer = Timer::init().unwrap();

        let mut c_timer = Completion::default();
        let mut c_cancel = Completion::default();

        // Add the timer
        let mut canceled = false;
        unsafe { timer.run(&mut l, &mut c_timer, 100_000, &mut canceled, set_true) };

        // Wait
        l.run(RunMode::NoWait).unwrap();
        assert!(!canceled);

        // Reset it
        unsafe {
            timer.reset(
                &mut l,
                &mut c_timer,
                &mut c_cancel,
                1,
                &mut canceled,
                set_true,
            )
        };

        l.run(RunMode::UntilDone).unwrap();
        assert!(canceled);
        assert_eq!(c_timer.state(), CompletionState::Dead);
        assert_eq!(c_cancel.state(), CompletionState::Dead);
    }

    #[test]
    fn timer_reset_before_tick() {
        let mut l = Loop::init(Options::default()).unwrap();
        let timer = Timer::init().unwrap();

        let mut c_timer = Completion::default();
        let mut c_cancel = Completion::default();

        // Add the timer
        let mut canceled = false;
        unsafe { timer.run(&mut l, &mut c_timer, 100_000, &mut canceled, set_true) };

        // Reset it
        unsafe {
            timer.reset(
                &mut l,
                &mut c_timer,
                &mut c_cancel,
                1,
                &mut canceled,
                set_true,
            )
        };

        l.run(RunMode::UntilDone).unwrap();
        assert!(canceled);
        assert_eq!(c_timer.state(), CompletionState::Dead);
        assert_eq!(c_cancel.state(), CompletionState::Dead);
    }

    #[test]
    fn timer_reset_after_trigger() {
        let mut l = Loop::init(Options::default()).unwrap();
        let timer = Timer::init().unwrap();

        let mut c_timer = Completion::default();
        let mut c_cancel = Completion::default();

        // Add the timer
        let mut canceled = false;
        unsafe { timer.run(&mut l, &mut c_timer, 1, &mut canceled, set_true) };
        l.run(RunMode::UntilDone).unwrap();
        assert!(canceled);
        canceled = false;

        // Reset it
        unsafe {
            timer.reset(
                &mut l,
                &mut c_timer,
                &mut c_cancel,
                1,
                &mut canceled,
                set_true,
            )
        };

        l.run(RunMode::UntilDone).unwrap();
        assert!(canceled);
        assert_eq!(c_timer.state(), CompletionState::Dead);
        assert_eq!(c_cancel.state(), CompletionState::Dead);
    }

    #[test]
    fn timer_cancel() {
        let mut l = Loop::init(Options::default()).unwrap();
        let timer = Timer::init().unwrap();

        // Add the timer
        let mut canceled = false;
        let mut c1 = Completion::default();
        unsafe {
            timer.run(&mut l, &mut c1, 100_000, &mut canceled, |ud, _, _, r| {
                // SAFETY: points to the live `canceled`.
                *ud = r == Err(RunError::Canceled);
                CallbackAction::Disarm
            })
        };

        // Cancel
        let mut cancel_confirm = false;
        let mut c2 = Completion::default();
        unsafe {
            timer.cancel(
                &mut l,
                &mut c1,
                &mut c2,
                &mut cancel_confirm,
                |ud: *mut bool, _, _, r| {
                    r.unwrap();
                    // SAFETY: points to the live `cancel_confirm`.
                    *ud = true;
                    CallbackAction::Disarm
                },
            )
        };

        // Wait
        l.run(RunMode::UntilDone).unwrap();
        assert!(canceled);
        assert!(cancel_confirm);
    }

    #[test]
    fn timer_rerun_from_callback() {
        // The termio/renderer pattern: the callback re-runs the timer on
        // the same completion and disarms.
        struct State {
            timer: Timer,
            count: u32,
        }
        fn cb(
            ud: *mut State,
            l: &mut Loop,
            c: &mut Completion,
            r: Result<(), RunError>,
        ) -> CallbackAction {
            r.unwrap();
            // SAFETY: points to the live State.
            let s = unsafe { &mut *ud };
            s.count += 1;
            if s.count < 3 {
                unsafe { s.timer.run(l, c, 1, ud, cb) };
            }
            CallbackAction::Disarm
        }

        let mut l = Loop::init(Options::default()).unwrap();
        let mut s = State {
            timer: Timer::init().unwrap(),
            count: 0,
        };
        let mut c = Completion::default();
        let timer = s.timer;
        unsafe { timer.run(&mut l, &mut c, 1, &mut s, cb) };
        l.run(RunMode::UntilDone).unwrap();
        assert_eq!(s.count, 3);
        assert_eq!(c.state(), CompletionState::Dead);
    }
}
