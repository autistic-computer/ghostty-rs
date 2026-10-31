//! Port of libxev `src/backend/epoll.zig`: the event loop, completions and
//! operations.
//!
//! Only the operations Ghostty needs are ported: `noop`, `cancel`, `poll`,
//! `read`, `write`, `close` and `timer`. The socket operations (`accept`,
//! `connect`, `send`, `recv`, `sendmsg`, `recvmsg`, `shutdown`), positional
//! file IO (`pread`, `pwrite`) and the thread pool are not.
//!
//! # Memory model
//!
//! Exactly like libxev, completions are *caller-owned* memory. Arming a
//! completion (any `unsafe fn` taking a `&mut Completion`, such as
//! [`Loop::add`], [`Loop::timer`] or the watcher APIs) stores a raw pointer
//! to it inside the loop. Until the completion is dead again (its callback
//! returned [`CallbackAction::Disarm`] or it was canceled/stopped, see
//! [`Completion::state`]) the completion **must not be moved, dropped or
//! otherwise accessed** except through the `&mut Completion` the loop passes
//! to its callback. The same applies to any buffer referenced by a
//! [`ReadBuffer::Slice`]/[`WriteBuffer::Slice`] and to the userdata pointer.
//!
//! The [`Loop`] itself does not need to be pinned: nothing points back to it.

use std::collections::{BTreeMap, VecDeque};
use std::ffi::c_void;

use super::common::{noop_callback, Callback, CallbackAction, CompletionState, Options, RunMode};
use super::poller::{events, Event, Poller};
use super::posix::{self, CancelError, CloseError, Error, Fd, PollError, ReadError, WriteError};

/// The maximum number of events read from the kernel per tick.
const MAX_EVENTS: usize = 1024;

const NS_PER_MS: u64 = 1_000_000;
const NS_PER_S: u64 = 1_000_000_000;
const MS_PER_S: i64 = 1_000;

/// True if epoll is available on this platform.
pub fn available() -> bool {
    cfg!(target_os = "linux")
}

#[derive(Debug, Default, Clone, Copy)]
struct LoopFlags {
    /// True once it is initialized.
    init: bool,

    /// Whether we're in a run or not (to prevent nested runs).
    in_run: bool,

    /// Whether our loop is in a stopped state or not.
    stopped: bool,
}

/// Epoll backend.
///
/// The Zig original keeps an extra eventfd registered to wake the loop from
/// its thread pool; since the thread pool is not ported, neither is that
/// eventfd.
pub struct Loop {
    /// The readiness backend (epoll on Linux). `None` after `deinit`.
    poller: Option<Poller>,

    /// The number of active completions. This DOES NOT include completions that
    /// are queued in the submissions queue.
    active: usize,

    /// Our queue of submissions that we want to enqueue on the next tick.
    submissions: VecDeque<*mut Completion>,

    /// The queue for completions to delete from the epoll fd.
    deletions: VecDeque<*mut Completion>,

    /// Heap of timers, ordered by (deadline in ns, insertion sequence). The
    /// Zig original uses an intrusive pairing heap; a `BTreeMap` gives the
    /// same ordered extraction and O(log n) removal without intrusive fields.
    timers: BTreeMap<(u64, u64), *mut Completion>,

    /// Monotonic sequence used to break deadline ties in `timers`.
    timer_seq: u64,

    /// Cached time
    cached_now: libc::timespec,

    /// Some internal fields we can pack for better space.
    flags: LoopFlags,

    /// Scratch buffer for ready events.
    events: Vec<Event>,
}

// SAFETY: a Loop only holds raw pointers to completions that the caller
// promised to keep valid (see module docs). Ghostty creates loops on one
// thread and runs them on another; the loop is never used from two threads
// at once (that would require `&mut` from both).
unsafe impl Send for Loop {}

impl Loop {
    pub fn init(_options: Options) -> Result<Loop, Error> {
        let mut res = Loop {
            poller: Some(Poller::new()?),
            active: 0,
            submissions: VecDeque::new(),
            deletions: VecDeque::new(),
            timers: BTreeMap::new(),
            timer_seq: 0,
            cached_now: libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            },
            flags: LoopFlags::default(),
            events: Vec::with_capacity(MAX_EVENTS),
        };
        res.update_now();
        Ok(res)
    }

    /// Release the loop's kernel resources. Safe to call more than once
    /// (it is also called on drop).
    pub fn deinit(&mut self) {
        self.poller = None;
    }

    fn poller(&mut self) -> &mut Poller {
        self.poller.as_mut().expect("xev loop used after deinit")
    }

    /// Run the event loop. See RunMode documentation for details on modes.
    pub fn run(&mut self, mode: RunMode) -> Result<(), Error> {
        match mode {
            RunMode::NoWait => self.tick(0),
            RunMode::Once => self.tick(1),
            RunMode::UntilDone => {
                while !self.done() {
                    self.tick(1)?;
                }
                Ok(())
            }
        }
    }

    fn done(&self) -> bool {
        self.flags.stopped || (self.active == 0 && self.submissions.is_empty())
    }

    /// Stop the loop. This can only be called from the main thread.
    /// This will stop the loop forever. Future ticks will do nothing.
    ///
    /// All completions are safe to read/write once any outstanding `run` or
    /// `tick` calls are returned.
    pub fn stop(&mut self) {
        self.flags.stopped = true;
    }

    /// Returns true if the loop is stopped. This may mean there
    /// are still pending completions to be processed.
    pub fn stopped(&self) -> bool {
        self.flags.stopped
    }

    /// Add a completion to the loop.
    ///
    /// # Safety
    ///
    /// `completion` must point to a valid completion that stays at the same
    /// address and is not otherwise accessed until it is dead again (see the
    /// module documentation), along with any buffers and userdata it refers
    /// to.
    pub unsafe fn add(&mut self, completion: *mut Completion) {
        // SAFETY: caller guarantees validity.
        let c = unsafe { &mut *completion };
        match c.flags.state {
            // Already adding, forget about it.
            State::Adding => return,

            // If it is dead we're good. If we're deleting we'll ignore it
            // while we're processing.
            State::Dead | State::Deleting => {}

            State::Active => unreachable!("cannot add an active completion"),
        }
        c.flags.state = State::Adding;

        // We just add the completion to the queue. Failures can happen
        // at tick time...
        self.submissions.push_back(completion);
    }

    /// Delete a completion from the loop.
    ///
    /// # Safety
    ///
    /// Same requirements as [`Loop::add`]: the completion must stay valid
    /// until the next loop tick has processed the deletion.
    pub unsafe fn delete(&mut self, completion: *mut Completion) {
        // SAFETY: caller guarantees validity.
        let c = unsafe { &mut *completion };
        match c.flags.state {
            // Already deleted
            State::Deleting => return,

            // If we're active then we will stop it and remove from epoll.
            // If we're adding then we'll ignore it when adding.
            State::Dead | State::Active | State::Adding => {}
        }
        // Remember whether the completion is registered (and counted in
        // `active`) so that the deletion can undo that. The Zig original
        // loses this information and leaks an `active` count.
        if c.flags.state == State::Active {
            c.flags.registered = true;
        }
        c.flags.state = State::Deleting;

        self.deletions.push_back(completion);
    }

    /// Returns the "loop" time in milliseconds. The loop time is updated
    /// once per loop tick, before IO polling occurs. It remains constant
    /// throughout callback execution.
    ///
    /// You can force an update of the "now" value by calling update_now()
    /// at any time from the main thread.
    ///
    /// The clock that is used is not guaranteed. In general, a monotonic
    /// clock source is always used if available. This value should typically
    /// just be used for relative time calculations within the loop, such as
    /// answering the question "did this happen <x> ms ago?".
    pub fn now(&self) -> i64 {
        // If anything overflows we just return the max value.
        let Some(sec) = (self.cached_now.tv_sec as i64).checked_mul(MS_PER_S) else {
            return i64::MAX;
        };
        let nsec = (self.cached_now.tv_nsec as i64).div_euclid(NS_PER_MS as i64);
        sec.saturating_add(nsec)
    }

    /// Update the cached time.
    pub fn update_now(&mut self) {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: valid out pointer.
        if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) } == 0 {
            self.cached_now = ts;
        }
    }

    /// The cached time in nanoseconds (saturating).
    fn cached_now_ns(&self) -> u64 {
        (self.cached_now.tv_sec as u64)
            .saturating_mul(NS_PER_S)
            .saturating_add(self.cached_now.tv_nsec as u64)
    }

    /// Add a timer to the loop. The timer will execute in "next_ms". This
    /// is oneshot: the timer will not repeat. To repeat a timer, either
    /// schedule another in your callback or return rearm from the callback.
    ///
    /// # Safety
    ///
    /// See [`Loop::add`].
    pub unsafe fn timer(
        &mut self,
        c: *mut Completion,
        next_ms: u64,
        userdata: *mut c_void,
        cb: Callback,
    ) {
        let next = self.timer_next(next_ms);
        // SAFETY: caller guarantees validity.
        unsafe {
            *c = Completion::new(Operation::Timer(TimerOp::new(next)), userdata, cb);
            self.add(c);
        }
    }

    /// Reset a timer to execute in "next_ms". If the timer is already
    /// started, it is stopped (without calling its callback) and restarted.
    /// If the timer has never been started (or already fired), this is
    /// equivalent to [`Loop::timer`]. In every case, the timer callback is
    /// updated to the given userdata and callback.
    ///
    /// This requires an additional completion `c_cancel` to represent the
    /// need to possibly cancel the previous timer. You can check if c_cancel
    /// was used by checking the state() after the call.
    ///
    /// # Safety
    ///
    /// See [`Loop::add`]; applies to both `c` and `c_cancel`.
    pub unsafe fn timer_reset(
        &mut self,
        c: *mut Completion,
        c_cancel: *mut Completion,
        next_ms: u64,
        userdata: *mut c_void,
        cb: Callback,
    ) {
        // SAFETY: caller guarantees validity.
        let state = unsafe { (*c).flags.state };
        match state {
            State::Dead | State::Deleting => {
                // SAFETY: forwarded caller guarantee.
                unsafe { self.timer(c, next_ms, userdata, cb) };
            }

            // Adding state we can just modify the metadata and return
            // since the timer isn't in the heap yet.
            State::Adding => {
                let next = self.timer_next(next_ms);
                // SAFETY: caller guarantees validity.
                let cr = unsafe { &mut *c };
                if let Operation::Timer(t) = &mut cr.op {
                    t.next = next;
                }
                cr.userdata = userdata;
                cr.callback = cb;
            }

            State::Active => {
                // Update the reset time for the timer to the desired time
                // along with all the callbacks.
                let next = self.timer_next(next_ms);
                // SAFETY: caller guarantees validity.
                let cr = unsafe { &mut *c };
                if let Operation::Timer(t) = &mut cr.op {
                    t.reset = Some(next);
                }
                cr.userdata = userdata;
                cr.callback = cb;

                // If the cancellation is active, we assume its for this timer
                // and do nothing.
                // SAFETY: caller guarantees validity.
                let cancel = unsafe { &mut *c_cancel };
                if cancel.state() == CompletionState::Active {
                    return;
                }
                debug_assert!(cancel.state() == CompletionState::Dead);
                *cancel =
                    Completion::new(Operation::Cancel { c }, std::ptr::null_mut(), noop_callback);
                // SAFETY: forwarded caller guarantee.
                unsafe { self.add(c_cancel) };
            }
        }
    }

    /// The absolute deadline (ns) for a timer firing in `next_ms`.
    fn timer_next(&self, next_ms: u64) -> u64 {
        // There are lots of failure scenarios here in math. If we see any
        // of them we just use the maximum value.
        self.cached_now_ns()
            .saturating_add(next_ms.saturating_mul(NS_PER_MS))
    }

    /// Tick through the event loop once, waiting for at least "wait" completions
    /// to be processed by the loop itself.
    pub fn tick(&mut self, wait: u32) -> Result<(), Error> {
        // If we're stopped then the loop is fully over.
        if self.flags.stopped {
            return Ok(());
        }

        // We can't nest runs.
        if self.flags.in_run {
            return Err(Error::NestedRunsNotAllowed);
        }
        self.flags.in_run = true;
        let res = self.tick_inner(wait);
        self.flags.in_run = false;
        res
    }

    fn tick_inner(&mut self, wait: u32) -> Result<(), Error> {
        // Initialize
        if !self.flags.init {
            self.flags.init = true;
        }

        // Submit all the submissions. We copy the submission queue so that
        // any resubmits don't cause an infinite loop.
        let mut wait_rem = wait as usize;
        let queued = std::mem::take(&mut self.submissions);
        for c in queued {
            // SAFETY: completions in the queue are valid (add's contract).
            let cr = unsafe { &mut *c };

            // We ignore any completions that aren't in the adding state.
            // This usually means that we switched them to be deleted or
            // something.
            if cr.flags.state != State::Adding {
                continue;
            }

            // These operations happen synchronously. Ensure they are
            // decremented from wait_rem.
            if matches!(cr.op, Operation::Cancel { .. } | Operation::Timer(_)) {
                wait_rem = wait_rem.saturating_sub(1);
            }

            self.start(c);
        }

        // Handle all deletions so we don't wait for them.
        while let Some(c) = self.deletions.pop_front() {
            // SAFETY: completions in the queue are valid (delete's contract).
            if unsafe { (*c).flags.state } != State::Deleting {
                continue;
            }
            self.stop_completion(c);
        }

        // If we have no active handles then we return no matter what.
        if self.active == 0 {
            // We still have to update our concept of "now".
            self.update_now();
            return Ok(());
        }

        // Wait and process events. We only do this if we have any active.
        let mut events = std::mem::take(&mut self.events);
        while self.active > 0 && (wait == 0 || wait_rem > 0) {
            self.update_now();
            let now_ns = self.cached_now_ns();

            // Run our expired timers
            while let Some((&key, &c)) = self.timers.first_key_value() {
                if key.0 >= now_ns {
                    break;
                }

                // Remove the timer
                self.timers.remove(&key);

                // SAFETY: armed timers are valid (add's contract).
                let cr = unsafe { &mut *c };
                if let Operation::Timer(t) = &mut cr.op {
                    t.heap_key = None;
                }

                // Mark completion as done
                cr.flags.state = State::Dead;
                self.active -= 1;

                // Lower our remaining count
                wait_rem = wait_rem.saturating_sub(1);

                // Invoke
                let action = self.invoke(c, OpResult::Timer(Ok(TimerTrigger::Expiration)));
                match action {
                    CallbackAction::Disarm => {}
                    CallbackAction::Rearm => self.start(c),
                }
            }

            // Determine our next timeout based on the timers
            let timeout: i32 = if wait_rem == 0 {
                0
            } else {
                // If we have a timer, we want to set the timeout to our next
                // timer value. If we have no timer, we wait forever.
                match self.timers.first_key_value() {
                    None => -1,
                    Some((&(next, _), _)) => {
                        // Determine the time in milliseconds.
                        let ms_now = now_ns / NS_PER_MS;
                        let ms_next = next / NS_PER_MS;
                        i32::try_from(ms_next.saturating_sub(ms_now)).unwrap_or(i32::MAX)
                    }
                }
            };

            // If timers disarmed everything, there is nothing to wait for.
            if self.active == 0 {
                break;
            }

            self.poller().wait(&mut events, MAX_EVENTS, timeout);
            let n = events.len();

            // Process all our events and invoke their completion handlers
            for ev in events.iter() {
                let c = ev.token as usize as *mut Completion;

                // We get the fd and mark this as in progress we can properly
                // clean this up later.
                // SAFETY: registered completions are valid (add's contract).
                let (fd, close_dup, res) = unsafe {
                    let cr = &mut *c;
                    let fd = if cr.flags.dup {
                        Some(cr.flags.dup_fd)
                    } else {
                        cr.fd()
                    };
                    let close_dup = cr.flags.dup;
                    cr.flags.state = State::Dead;
                    (fd, close_dup, cr.perform())
                };

                let action = self.invoke(c, res);
                match action {
                    CallbackAction::Disarm => {
                        // We can't use self.stop because we can't trust
                        // that c is still a valid pointer.
                        if let Some(v) = fd {
                            if let Err(err) = self.poller().del(v) {
                                log::warn!(target: "xev", "epoll_ctl DEL failed fd={v} err={err}");
                            }
                            if close_dup {
                                posix::close(v);
                            }
                        }

                        self.active -= 1;
                    }

                    // For epoll, epoll remains armed by default. We have to
                    // reset the state, that is all.
                    // SAFETY: the callback rearmed, so c is still valid.
                    CallbackAction::Rearm => unsafe { (*c).flags.state = State::Active },
                }
            }

            if wait == 0 {
                break;
            }
            wait_rem = wait_rem.saturating_sub(n);
        }
        self.events = events;

        Ok(())
    }

    /// Call the completion's callback.
    fn invoke(&mut self, c: *mut Completion, res: OpResult) -> CallbackAction {
        // SAFETY: c is a valid armed completion (add's contract); the
        // reference only lives for the duration of the callback.
        let cr = unsafe { &mut *c };
        let (cb, ud) = (cr.callback, cr.userdata);
        cb(ud, self, cr, res)
    }

    fn start(&mut self, completion: *mut Completion) {
        let token = completion as usize as u64;
        // SAFETY: completion is valid (add's contract).
        let cr = unsafe { &mut *completion };

        let res: Option<OpResult> = match cr.op {
            Operation::Noop => {
                cr.flags.state = State::Dead;
                return;
            }

            Operation::Cancel { c: target } => {
                // We stop immediately. We only stop if we are in the
                // "adding" state because cancellation or any other action
                // means we're complete already.
                if cr.flags.state == State::Adding {
                    // SAFETY: the canceled completion must be valid too.
                    if matches!(unsafe { &(*target).op }, Operation::Cancel { .. }) {
                        panic!("cannot cancel a cancellation");
                    }
                    self.stop_completion(target);
                }

                // We always run timers
                Some(OpResult::Cancel(Ok(())))
            }

            Operation::Read { .. } => self
                .register(completion, events::IN | events::RDHUP, token)
                .err()
                .map(|e| OpResult::Read(Err(e))),

            Operation::Write { .. } => self
                .register(completion, events::OUT, token)
                .err()
                .map(|e| OpResult::Write(Err(e))),

            Operation::Poll { events: ev, .. } => self
                .register(completion, ev, token)
                .err()
                .map(|e| OpResult::Poll(Err(e))),

            Operation::Close { fd } => {
                posix::close(fd);
                Some(OpResult::Close(Ok(())))
            }

            Operation::Timer(_) => {
                // Insert the timer into our heap.
                self.timer_seq = self.timer_seq.wrapping_add(1);
                let seq = self.timer_seq;
                // SAFETY: completion is valid.
                let cr = unsafe { &mut *completion };
                if let Operation::Timer(t) = &mut cr.op {
                    let key = (t.next, seq);
                    t.heap_key = Some(key);
                    self.timers.insert(key, completion);
                }

                // We always run timers
                None
            }
        };

        // If we failed to add the completion then we call the callback
        // immediately and mark the error.
        if let Some(res) = res {
            // SAFETY: completion is valid.
            unsafe { (*completion).flags.state = State::Dead };

            match self.invoke(completion, res) {
                CallbackAction::Disarm => {}

                // If we rearm then we requeue this. Due to the way that tick works,
                // this won't try to re-add immediately it won't happen until the
                // next tick.
                // SAFETY: the callback rearmed so the completion is valid.
                CallbackAction::Rearm => unsafe { self.add(completion) },
            }

            return;
        }

        // Mark the completion as active if we reached this point
        // SAFETY: completion is valid.
        unsafe { (*completion).flags.state = State::Active };

        // Increase our active count
        self.active += 1;
    }

    /// Register the completion's fd (dup'd if requested) with the poller.
    fn register(&mut self, completion: *mut Completion, ev: u32, token: u64) -> Result<(), Error> {
        // SAFETY: completion is valid.
        let fd = unsafe { (*completion).fd_maybe_dup() }?;
        self.poller().add(fd, ev, token)
    }

    fn stop_completion(&mut self, completion: *mut Completion) {
        // SAFETY: completion is valid (add/delete/cancel contract).
        let cr = unsafe { &mut *completion };
        let was_active = cr.flags.state == State::Active
            || (cr.flags.state == State::Deleting && cr.flags.registered);
        cr.flags.registered = false;

        if cr.fd().is_some() {
            // Delete. The fd is only registered if we're active. If we
            // dup'd the fd we own the dup and close it (the Zig original
            // leaks it here).
            let registered = if cr.flags.dup {
                (cr.flags.dup_fd > 0).then_some(cr.flags.dup_fd)
            } else {
                cr.fd()
            };
            if let Some(fd) = registered {
                if was_active {
                    if let Err(err) = self.poller().del(fd) {
                        log::warn!(target: "xev", "epoll_ctl DEL failed fd={fd} err={err}");
                    }
                }
                if cr.flags.dup {
                    posix::close(fd);
                    cr.flags.dup_fd = 0;
                }
            }
        } else if let Operation::Timer(t) = &mut cr.op {
            if was_active {
                // Timers needs to be removed from the timer heap.
                if let Some(key) = t.heap_key.take() {
                    self.timers.remove(&key);
                }
            }

            // If the timer was never fired, we need to fire it with
            // the cancellation notice.
            if cr.flags.state != State::Dead {
                // If we have reset set AND we got a cancellation result,
                // that means that we were canceled so that we can update
                // our expiration time.
                if let Some(r) = t.reset.take() {
                    t.next = r;
                    if was_active {
                        self.active -= 1;
                    }
                    self.start(completion);
                    return;
                }

                let action = self.invoke(completion, OpResult::Timer(Ok(TimerTrigger::Cancel)));
                match action {
                    CallbackAction::Disarm => {}
                    CallbackAction::Rearm => {
                        if was_active {
                            self.active -= 1;
                        }
                        self.start(completion);
                        return;
                    }
                }
            }
        }

        // Decrement the active count so we know how many are running for
        // .until_done run semantics.
        // SAFETY: completion is valid.
        let cr = unsafe { &mut *completion };
        if was_active {
            self.active -= 1;
        }

        // Mark the completion as done, unless a cancellation callback
        // re-armed it (it is then queued in the "adding" state).
        if cr.flags.state != State::Adding {
            cr.flags.state = State::Dead;
        }
    }
}

impl Drop for Loop {
    fn drop(&mut self) {
        self.deinit();
    }
}

/// Watch state of a completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum State {
    /// completion is not part of any loop
    #[default]
    Dead = 0,

    /// completion is in the submission queue
    Adding = 1,

    /// completion is in the deletion queue
    Deleting = 2,

    /// completion is registered with epoll
    Active = 3,
}

#[derive(Debug, Clone, Copy, Default)]
struct CompletionFlags {
    /// Watch state of this completion. We use this to determine whether
    /// we're active, adding, deleting, etc. This lets us add and delete
    /// multiple times before a loop tick and handle the state properly.
    state: State,

    /// Set to true to dup the file descriptor for the operation prior
    /// to setting it up with epoll. This is a hack to make it so that
    /// a completion can represent a single op per fd, since epoll requires
    /// a single fd for multiple ops. We don't want to track that esp
    /// since epoll isn't the primary Linux interface.
    dup: bool,
    dup_fd: Fd,

    /// (Rust port) Set by `Loop::delete` when the completion being deleted
    /// was active.
    registered: bool,
}

pub struct Completion {
    /// Operation to execute. This is only safe to read BEFORE the completion
    /// is queued. After being queued (with "add"), the operation may change.
    pub op: Operation,

    /// Userdata and callback for when the completion is finished.
    pub userdata: *mut c_void,
    pub callback: Callback,

    //---------------------------------------------------------------
    // Internal fields
    flags: CompletionFlags,

    /// The typed callback of a high-level watcher (`Timer::run`,
    /// `Async::wait`, ...), type-erased. The Zig original bakes the typed
    /// callback into a comptime-generated wrapper; in Rust the generic
    /// wrapper (`callback`) reads it back from here.
    pub(crate) user_callback: *const (),

    /// Auxiliary per-watcher data (e.g. the pid of a polled process).
    pub(crate) aux: i64,
}

// SAFETY: see `Loop`. A completion is only touched by the thread that runs
// the loop it is armed on.
unsafe impl Send for Completion {}

impl Default for Completion {
    fn default() -> Self {
        Self::new(Operation::Noop, std::ptr::null_mut(), noop_callback)
    }
}

impl std::fmt::Debug for Completion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Completion")
            .field("op", &self.op)
            .field("state", &self.flags.state)
            .finish_non_exhaustive()
    }
}

impl Completion {
    /// Zig: `.{ .op = op, .userdata = userdata, .callback = callback }`.
    pub fn new(op: Operation, userdata: *mut c_void, callback: Callback) -> Self {
        Self {
            op,
            userdata,
            callback,
            flags: CompletionFlags::default(),
            user_callback: std::ptr::null(),
            aux: 0,
        }
    }

    /// Returns the state of this completion. There are some things to
    /// be caution about when calling this function.
    ///
    /// First, this is only safe to call from the main thread. This cannot
    /// be called from any other thread.
    ///
    /// Second, if you stop the loop (loop.stop()), the completions registered
    /// with the loop will NOT be reset to a dead state.
    pub fn state(&self) -> CompletionState {
        match self.flags.state {
            State::Dead => CompletionState::Dead,
            State::Adding | State::Deleting | State::Active => CompletionState::Active,
        }
    }

    /// Request that the fd be dup'd before registering it (see the epoll
    /// notes on `CompletionFlags::dup`).
    pub(crate) fn set_dup(&mut self, dup: bool) {
        self.flags.dup = dup;
    }

    /// Perform the operation associated with this completion. This will
    /// perform the full blocking operation for the completion.
    fn perform(&mut self) -> OpResult {
        match &mut self.op {
            // This should never happen because we always do these synchronously
            // or in another location.
            Operation::Cancel { .. } | Operation::Noop | Operation::Timer(_) => unreachable!(),

            Operation::Poll { .. } => OpResult::Poll(Ok(())),

            Operation::Read { fd, buffer } => {
                let n = match buffer {
                    // SAFETY: the slice buffer is valid per the arming contract.
                    ReadBuffer::Slice { ptr, len } => {
                        posix::read(*fd, unsafe { std::slice::from_raw_parts_mut(*ptr, *len) })
                    }
                    ReadBuffer::Array(a) => posix::read(*fd, a),
                };
                OpResult::Read(match n {
                    Ok(0) => Err(Error::EOF),
                    other => other,
                })
            }

            Operation::Write { fd, buffer } => {
                // SAFETY: the slice buffer is valid per the arming contract.
                OpResult::Write(posix::write(*fd, unsafe { buffer.as_slice() }))
            }

            Operation::Close { fd } => {
                posix::close(*fd);
                OpResult::Close(Ok(()))
            }
        }
    }

    /// Return the fd for the completion. This will perform a dup(2) if
    /// requested.
    fn fd_maybe_dup(&mut self) -> Result<Fd, Error> {
        let old_fd = self.fd().expect("operation has no fd");
        if !self.flags.dup {
            return Ok(old_fd);
        }
        if self.flags.dup_fd > 0 {
            return Ok(self.flags.dup_fd);
        }

        self.flags.dup_fd = posix::dup(old_fd).map_err(|_| Error::DupFailed)?;
        Ok(self.flags.dup_fd)
    }

    /// Returns the fd associated with the completion (if any).
    fn fd(&self) -> Option<Fd> {
        match &self.op {
            Operation::Poll { fd, .. }
            | Operation::Read { fd, .. }
            | Operation::Write { fd, .. }
            | Operation::Close { fd } => Some(*fd),
            Operation::Cancel { .. } | Operation::Timer(_) => None,
            Operation::Noop => unreachable!(),
        }
    }
}

/// Zig: `OperationType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationType {
    Noop,
    Cancel,
    Poll,
    Read,
    Write,
    Close,
    Timer,
}

/// The result type based on the operation type. For a callback, the
/// result tag will ALWAYS match the operation tag.
///
/// Zig: `xev.Result` (renamed so it does not shadow `std::result::Result`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpResult {
    Noop,
    Cancel(Result<(), CancelError>),
    Poll(Result<(), PollError>),
    Read(Result<usize, ReadError>),
    Write(Result<usize, WriteError>),
    Close(Result<(), CloseError>),
    Timer(Result<TimerTrigger, TimerError>),
}

impl OpResult {
    pub fn op_type(&self) -> OperationType {
        match self {
            OpResult::Noop => OperationType::Noop,
            OpResult::Cancel(_) => OperationType::Cancel,
            OpResult::Poll(_) => OperationType::Poll,
            OpResult::Read(_) => OperationType::Read,
            OpResult::Write(_) => OperationType::Write,
            OpResult::Close(_) => OperationType::Close,
            OpResult::Timer(_) => OperationType::Timer,
        }
    }
}

/// All the supported operations of this event loop. These are always
/// backend-specific and therefore the structure and types change depending
/// on the underlying system in use. The high level operations are
/// done by initializing the request handles.
#[derive(Debug, Clone, Copy)]
pub enum Operation {
    Noop,

    Cancel {
        c: *mut Completion,
    },

    /// Poll for events but do not perform any operations on them being
    /// ready. The "events" field are a OR-ed list of EPOLL events (see
    /// [`super::poller::events`]).
    Poll {
        fd: Fd,
        events: u32,
    },

    Read {
        fd: Fd,
        buffer: ReadBuffer,
    },

    Write {
        fd: Fd,
        buffer: WriteBuffer,
    },

    Close {
        fd: Fd,
    },

    Timer(TimerOp),
}

impl Operation {
    pub fn op_type(&self) -> OperationType {
        match self {
            Operation::Noop => OperationType::Noop,
            Operation::Cancel { .. } => OperationType::Cancel,
            Operation::Poll { .. } => OperationType::Poll,
            Operation::Read { .. } => OperationType::Read,
            Operation::Write { .. } => OperationType::Write,
            Operation::Close { .. } => OperationType::Close,
            Operation::Timer(_) => OperationType::Timer,
        }
    }
}

/// Zig: `Operation.Timer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimerOp {
    /// The absolute time (monotonic, ns) to fire this timer next.
    pub next: u64,

    /// Only used internally. If this is non-null and timer is
    /// CANCELLED, then the timer is rearmed automatically with this
    /// as the next time. The callback will not be called on the
    /// cancellation.
    pub reset: Option<u64>,

    /// Internal heap key (Zig: intrusive heap field).
    heap_key: Option<(u64, u64)>,
}

impl TimerOp {
    pub fn new(next: u64) -> Self {
        Self {
            next,
            reset: None,
            heap_key: None,
        }
    }
}

/// ReadBuffer are the various options for reading.
#[derive(Debug, Clone, Copy)]
pub enum ReadBuffer {
    /// Read into this slice (raw parts; the memory must stay valid while
    /// the read is armed).
    Slice { ptr: *mut u8, len: usize },

    /// Read into this array, just set this to zero and it will
    /// be populated up to the size of the array. This is an option because
    /// the other union members force a specific size anyways so this lets us
    /// use the other size in the union to support small reads without worrying
    /// about buffer allocation.
    ///
    /// To know the size read you have to use the return value of the
    /// read operations (i.e. recv).
    Array([u8; 32]),
}

impl ReadBuffer {
    /// Zig: `.{ .slice = buf }`.
    pub fn slice(buf: &mut [u8]) -> Self {
        ReadBuffer::Slice {
            ptr: buf.as_mut_ptr(),
            len: buf.len(),
        }
    }
}

/// WriteBuffer are the various options for writing.
#[derive(Debug, Clone, Copy)]
pub enum WriteBuffer {
    /// Write from this buffer (raw parts; the memory must stay valid while
    /// the write is armed).
    Slice { ptr: *const u8, len: usize },

    /// Write from this array. See ReadBuffer.array for why we support this.
    Array { array: [u8; 32], len: usize },
}

impl WriteBuffer {
    /// Zig: `.{ .slice = buf }`.
    pub fn slice(buf: &[u8]) -> Self {
        WriteBuffer::Slice {
            ptr: buf.as_ptr(),
            len: buf.len(),
        }
    }

    /// The number of bytes in the buffer.
    pub fn len(&self) -> usize {
        match self {
            WriteBuffer::Slice { len, .. } => *len,
            WriteBuffer::Array { len, .. } => *len,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The bytes of the buffer.
    ///
    /// # Safety
    ///
    /// For [`WriteBuffer::Slice`], the referenced memory must still be valid.
    pub unsafe fn as_slice(&self) -> &[u8] {
        match self {
            WriteBuffer::Slice { ptr, len } => {
                if *len == 0 {
                    &[]
                } else {
                    // SAFETY: caller guarantees validity.
                    unsafe { std::slice::from_raw_parts(*ptr, *len) }
                }
            }
            WriteBuffer::Array { array, len } => &array[..*len],
        }
    }
}

/// Zig: `TimerError`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerError {
    Unexpected,
}

impl std::fmt::Display for TimerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Unexpected")
    }
}

impl std::error::Error for TimerError {}

/// Zig: `TimerTrigger`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerTrigger {
    /// Unused with epoll
    Request,

    /// Timer expired.
    Expiration,

    /// Timer was canceled.
    Cancel,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_bool(ud: *mut c_void, _: &mut Loop, _: &mut Completion, _: OpResult) -> CallbackAction {
        // SAFETY: tests pass a pointer to a live bool.
        unsafe { *(ud as *mut bool) = true };
        CallbackAction::Disarm
    }

    fn set_trigger(
        ud: *mut c_void,
        _: &mut Loop,
        _: &mut Completion,
        r: OpResult,
    ) -> CallbackAction {
        let OpResult::Timer(t) = r else {
            unreachable!()
        };
        // SAFETY: tests pass a pointer to a live Option<TimerTrigger>.
        unsafe { *(ud as *mut Option<TimerTrigger>) = Some(t.unwrap()) };
        CallbackAction::Disarm
    }

    fn ptr<T>(v: &mut T) -> *mut c_void {
        v as *mut T as *mut c_void
    }

    #[test]
    fn epoll_default_completion() {
        let mut l = Loop::init(Options::default()).unwrap();
        let mut c = Completion::default();
        unsafe { l.add(&mut c) };

        // Tick
        l.run(RunMode::UntilDone).unwrap();

        // Completion should be dead.
        assert_eq!(c.state(), CompletionState::Dead);
    }

    #[test]
    fn epoll_loop_time() {
        let mut l = Loop::init(Options::default()).unwrap();

        // should never init zero
        let now = l.now();
        assert!(now > 0);

        // should update on a loop tick
        while now == l.now() {
            l.run(RunMode::NoWait).unwrap();
        }
    }

    #[test]
    fn epoll_stop() {
        let mut l = Loop::init(Options::default()).unwrap();

        // Add the timer
        let mut called = false;
        let mut c1 = Completion::default();
        unsafe { l.timer(&mut c1, 1_000_000, ptr(&mut called), set_bool) };

        // Tick
        l.run(RunMode::NoWait).unwrap();
        assert!(!called);

        // Stop
        l.stop();
        l.run(RunMode::UntilDone).unwrap();
        assert!(!called);
    }

    #[test]
    fn epoll_timer() {
        let mut l = Loop::init(Options::default()).unwrap();

        // Add the timer
        let mut called = false;
        let mut c1 = Completion::default();
        unsafe { l.timer(&mut c1, 1, ptr(&mut called), set_bool) };

        // Add another timer
        let mut called2 = false;
        let mut c2 = Completion::default();
        unsafe { l.timer(&mut c2, 100_000, ptr(&mut called2), set_bool) };

        // State checking
        assert_eq!(c1.state(), CompletionState::Active);
        assert_eq!(c2.state(), CompletionState::Active);

        // Tick
        while !unsafe { std::ptr::read_volatile(&called) } {
            l.run(RunMode::NoWait).unwrap();
        }
        assert!(called);
        assert!(!called2);

        // State checking
        assert_eq!(c1.state(), CompletionState::Dead);
        assert_eq!(c2.state(), CompletionState::Active);
    }

    #[test]
    fn epoll_timer_ordering() {
        // Timers fire in deadline order, not insertion order.
        fn push(ud: *mut c_void, _: &mut Loop, c: &mut Completion, _: OpResult) -> CallbackAction {
            // SAFETY: tests pass a pointer to a live Vec<u64>.
            let v = unsafe { &mut *(ud as *mut Vec<u64>) };
            let Operation::Timer(t) = c.op else {
                unreachable!()
            };
            v.push(t.next);
            CallbackAction::Disarm
        }

        let mut l = Loop::init(Options::default()).unwrap();
        let mut order: Vec<u64> = Vec::new();
        let mut cs: Vec<Completion> = (0..4).map(|_| Completion::default()).collect();
        let delays = [30u64, 10, 20, 0];
        for (c, d) in cs.iter_mut().zip(delays) {
            unsafe { l.timer(c, d, ptr(&mut order), push) };
        }
        l.run(RunMode::UntilDone).unwrap();
        assert_eq!(order.len(), 4);
        let mut sorted = order.clone();
        sorted.sort();
        assert_eq!(order, sorted);
    }

    #[test]
    fn epoll_timer_reset() {
        let mut l = Loop::init(Options::default()).unwrap();

        // Add the timer
        let mut trigger: Option<TimerTrigger> = None;
        let mut c1 = Completion::default();
        unsafe { l.timer(&mut c1, 100_000, ptr(&mut trigger), set_trigger) };

        // We know timer won't be called from the timer test previously.
        l.run(RunMode::NoWait).unwrap();
        assert!(trigger.is_none());

        // Reset the timer
        let mut c_cancel = Completion::default();
        unsafe { l.timer_reset(&mut c1, &mut c_cancel, 1, ptr(&mut trigger), set_trigger) };
        assert_eq!(c1.state(), CompletionState::Active);
        assert_eq!(c_cancel.state(), CompletionState::Active);

        // Run
        l.run(RunMode::UntilDone).unwrap();
        assert_eq!(trigger, Some(TimerTrigger::Expiration));
        assert_eq!(c1.state(), CompletionState::Dead);
        assert_eq!(c_cancel.state(), CompletionState::Dead);
    }

    #[test]
    fn epoll_timer_reset_before_tick() {
        let mut l = Loop::init(Options::default()).unwrap();

        // Add the timer
        let mut trigger: Option<TimerTrigger> = None;
        let mut c1 = Completion::default();
        unsafe { l.timer(&mut c1, 100_000, ptr(&mut trigger), set_trigger) };

        // Reset the timer
        let mut c_cancel = Completion::default();
        unsafe { l.timer_reset(&mut c1, &mut c_cancel, 1, ptr(&mut trigger), set_trigger) };
        assert_eq!(c1.state(), CompletionState::Active);
        assert_eq!(c_cancel.state(), CompletionState::Dead);

        // Run
        l.run(RunMode::UntilDone).unwrap();
        assert_eq!(trigger, Some(TimerTrigger::Expiration));
        assert_eq!(c1.state(), CompletionState::Dead);
        assert_eq!(c_cancel.state(), CompletionState::Dead);
    }

    #[test]
    fn epoll_timer_reset_after_trigger() {
        let mut l = Loop::init(Options::default()).unwrap();

        // Add the timer
        let mut trigger: Option<TimerTrigger> = None;
        let mut c1 = Completion::default();
        unsafe { l.timer(&mut c1, 1, ptr(&mut trigger), set_trigger) };

        // Run the timer
        l.run(RunMode::UntilDone).unwrap();
        assert_eq!(trigger, Some(TimerTrigger::Expiration));
        assert_eq!(c1.state(), CompletionState::Dead);
        trigger = None;

        // Reset the timer
        let mut c_cancel = Completion::default();
        unsafe { l.timer_reset(&mut c1, &mut c_cancel, 1, ptr(&mut trigger), set_trigger) };
        assert_eq!(c1.state(), CompletionState::Active);
        assert_eq!(c_cancel.state(), CompletionState::Dead);

        // Run
        l.run(RunMode::UntilDone).unwrap();
        assert_eq!(trigger, Some(TimerTrigger::Expiration));
        assert_eq!(c1.state(), CompletionState::Dead);
        assert_eq!(c_cancel.state(), CompletionState::Dead);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn epoll_timerfd() {
        let mut l = Loop::init(Options::default()).unwrap();

        // We'll try with a simple timerfd
        // SAFETY: plain syscalls with valid arguments.
        let fd = unsafe { libc::timerfd_create(libc::CLOCK_MONOTONIC, libc::TFD_CLOEXEC) };
        assert!(fd >= 0);
        let spec = libc::itimerspec {
            it_interval: libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            },
            it_value: libc::timespec {
                tv_sec: 0,
                tv_nsec: 1,
            },
        };
        assert_eq!(
            unsafe { libc::timerfd_settime(fd, 0, &spec, std::ptr::null_mut()) },
            0
        );

        fn cb(ud: *mut c_void, _: &mut Loop, _: &mut Completion, r: OpResult) -> CallbackAction {
            let OpResult::Read(r) = r else { unreachable!() };
            r.unwrap();
            // SAFETY: tests pass a pointer to a live bool.
            unsafe { *(ud as *mut bool) = true };
            CallbackAction::Disarm
        }

        // Add the timer
        let mut called = false;
        let mut c = Completion::new(
            Operation::Read {
                fd,
                buffer: ReadBuffer::Array([0; 32]),
            },
            ptr(&mut called),
            cb,
        );
        unsafe { l.add(&mut c) };

        // Tick
        l.run(RunMode::UntilDone).unwrap();
        assert!(called);
        posix::close(fd);
    }

    #[test]
    fn epoll_timer_cancellation() {
        let mut l = Loop::init(Options::default()).unwrap();

        // Add the timer
        let mut trigger: Option<TimerTrigger> = None;
        let mut c1 = Completion::default();
        unsafe { l.timer(&mut c1, 100_000, ptr(&mut trigger), set_trigger) };

        // Tick and verify we're not called.
        l.run(RunMode::NoWait).unwrap();
        assert!(trigger.is_none());

        fn cancel_cb(
            ud: *mut c_void,
            _: &mut Loop,
            _: &mut Completion,
            r: OpResult,
        ) -> CallbackAction {
            let OpResult::Cancel(r) = r else {
                unreachable!()
            };
            r.unwrap();
            // SAFETY: tests pass a pointer to a live bool.
            unsafe { *(ud as *mut bool) = true };
            CallbackAction::Disarm
        }

        // Cancel the timer
        let mut called = false;
        let mut c_cancel = Completion::new(
            Operation::Cancel { c: &mut c1 },
            ptr(&mut called),
            cancel_cb,
        );
        unsafe { l.add(&mut c_cancel) };

        // Tick
        l.run(RunMode::UntilDone).unwrap();
        assert!(called);
        assert_eq!(trigger, Some(TimerTrigger::Cancel));
    }

    #[test]
    fn epoll_canceling_a_completed_operation() {
        let mut l = Loop::init(Options::default()).unwrap();

        // Add the timer
        let mut trigger: Option<TimerTrigger> = None;
        let mut c1 = Completion::default();
        unsafe { l.timer(&mut c1, 1, ptr(&mut trigger), set_trigger) };

        // Tick and verify we're not called.
        l.run(RunMode::UntilDone).unwrap();
        assert_eq!(trigger, Some(TimerTrigger::Expiration));

        fn cancel_cb(
            ud: *mut c_void,
            _: &mut Loop,
            _: &mut Completion,
            r: OpResult,
        ) -> CallbackAction {
            let OpResult::Cancel(r) = r else {
                unreachable!()
            };
            r.unwrap();
            // SAFETY: tests pass a pointer to a live bool.
            unsafe { *(ud as *mut bool) = true };
            CallbackAction::Disarm
        }

        // Cancel the timer
        let mut called = false;
        let mut c_cancel = Completion::new(
            Operation::Cancel { c: &mut c1 },
            ptr(&mut called),
            cancel_cb,
        );
        unsafe { l.add(&mut c_cancel) };

        // Tick
        l.run(RunMode::UntilDone).unwrap();
        assert!(called);
        assert_eq!(trigger, Some(TimerTrigger::Expiration));
    }

    #[test]
    fn epoll_timer_rearm() {
        // Rearming a timer re-inserts it with the same (now past) deadline,
        // so it fires again on the next tick.
        fn cb(ud: *mut c_void, _: &mut Loop, _: &mut Completion, r: OpResult) -> CallbackAction {
            assert_eq!(r, OpResult::Timer(Ok(TimerTrigger::Expiration)));
            // SAFETY: tests pass a pointer to a live u32.
            let n = unsafe { &mut *(ud as *mut u32) };
            *n += 1;
            if *n < 3 {
                CallbackAction::Rearm
            } else {
                CallbackAction::Disarm
            }
        }

        let mut l = Loop::init(Options::default()).unwrap();
        let mut count: u32 = 0;
        let mut c = Completion::default();
        unsafe { l.timer(&mut c, 1, ptr(&mut count), cb) };
        l.run(RunMode::UntilDone).unwrap();
        assert_eq!(count, 3);
        assert_eq!(c.state(), CompletionState::Dead);
    }

    #[test]
    fn epoll_delete_pending_read() {
        // Deleting an armed fd completion unregisters it without a callback.
        let mut fds = [0; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);

        let mut l = Loop::init(Options::default()).unwrap();
        let mut called = false;
        let mut c = Completion::new(
            Operation::Read {
                fd: fds[0],
                buffer: ReadBuffer::Array([0; 32]),
            },
            ptr(&mut called),
            set_bool,
        );
        unsafe { l.add(&mut c) };
        l.run(RunMode::NoWait).unwrap();
        assert_eq!(c.state(), CompletionState::Active);

        unsafe { l.delete(&mut c) };
        l.run(RunMode::UntilDone).unwrap();
        assert_eq!(c.state(), CompletionState::Dead);
        assert!(!called);

        posix::close(fds[0]);
        posix::close(fds[1]);
    }
}
