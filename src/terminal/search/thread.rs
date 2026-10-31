//! Port of `src/terminal/search/Thread.zig`.
//!
//! STATUS: ported
//!
//! Search thread that handles searching a terminal for a string match.
//! This is expected to run on a dedicated thread to try to prevent too much
//! overhead to other terminal read/write operations.
//!
//! The current architecture of search does acquire global locks for accessing
//! terminal data, so there's still added contention, but we do our best to
//! minimize this by trading off memory usage (copying data to minimize lock
//! time).
//!
//! Notes on the port:
//! - The mutex (`std.Io.Mutex`, here [`crate::lib::tinyio::Mutex`]) and the
//!   terminal are raw pointers like in Zig; the caller guarantees they
//!   outlive the thread. `global.io()` is the zero-sized [`TinyIo`].
//! - The mailbox is a `Box` owned by the thread (Zig: created/destroyed
//!   with the allocator); `deinit` frees it with the thread.
//! - Event payloads borrow search-owned memory: [`Event`] carries a
//!   lifetime and the selected match highlight is a reference (Zig: a
//!   shallow copy that is only valid during the callback).
//! - `threadMain` runs on `&mut self` like the renderer thread; other
//!   threads may only use the mailbox and copies of the async handles
//!   while it runs.

use core::ffi::c_void;
use core::ptr::NonNull;

use crate::datastruct::blocking_queue::BlockingQueue;
use crate::datastruct::message_data::MessageData;
use crate::global::xev;
use crate::lib::tinyio::{Mutex, TinyIo};
use crate::terminal::highlight::{Flattened as FlattenedHighlight, Untracked as UntrackedHighlight};
use crate::terminal::page_list::Allocator;
use crate::terminal::screen_set::Key as ScreenKey;
use crate::terminal::search::screen::Select;
use crate::terminal::search::terminal::{SelectScroll, TerminalSearch, Tick};
use crate::terminal::size::AllocError;
use crate::terminal::terminal::Terminal;

const LOG: &str = "search_thread";

// TODO: Some stuff that could be improved:
// - pause the refresh timer when the terminal isn't focused
// - we probably want to know our progress through the search
//   for viewport matches so we can show n/total UI.
// - notifications should be coalesced to avoid spamming a massive
//   amount of events if the terminal is changing rapidly.

/// The interval at which we refresh the terminal state to check if
/// there are any changes that require us to re-search. This should be
/// balanced to be fast enough to be responsive but not so fast that
/// we hold the terminal lock too often.
const REFRESH_INTERVAL: u64 = 24; // 40 FPS

/// See the module documentation.
pub struct Thread {
    /// Allocator used for some state
    pub alloc: Allocator,

    /// The mailbox that can be used to send this thread messages. Note
    /// this is a blocking queue so if it is full you will get errors (or block).
    pub mailbox: Box<Mailbox>,

    /// The event loop for the search thread.
    pub r#loop: xev::Loop,

    /// This can be used to wake up the renderer and force a render safely from
    /// any thread.
    pub wakeup: xev::Async,
    pub wakeup_c: xev::Completion,

    /// This can be used to stop the thread on the next loop iteration.
    pub stop: xev::Async,
    pub stop_c: xev::Completion,

    /// The timer used for refreshing the terminal state to determine if
    /// we have a stale active area, viewport, screen change, etc. This is
    /// CPU intensive so we stop doing this under certain conditions.
    pub refresh: xev::Timer,
    pub refresh_c: xev::Completion,
    pub refresh_active: bool,

    /// Search state. Starts as null and is populated when a search is
    /// started (a needle is given).
    pub search: Option<TerminalSearch>,

    /// The last-notified values used to diff search state into events.
    notify_state: NotifyState,

    /// The options used to initialize this thread.
    pub opts: Options,
}

/// (Zig: `Thread.Options`.)
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// Mutex that must be held while reading/writing the terminal.
    pub mutex: NonNull<Mutex>,

    /// The terminal data to search.
    pub terminal: NonNull<Terminal>,

    /// The callback for events from the search thread along with optional
    /// userdata. This can be null if you don't want to receive events,
    /// which could be useful for a one-time search (although, odd, you
    /// should use our search structures directly then).
    pub event_cb: Option<EventCallback>,
    pub event_userdata: *mut c_void,
}

/// (Zig: `Thread.EventCallback`.)
pub type EventCallback = fn(event: Event<'_>, userdata: *mut c_void);

/// The type used for sending messages to the thread.
pub type Mailbox = BlockingQueue<Message, 64>;

/// Represents a write request. Magic number comes from the max size
/// we want this union to be. (Zig: `Message.WriteReq`.)
pub type WriteReq = MessageData<u8, 255>;

/// The messages that can be sent to the thread.
#[derive(Debug)]
pub enum Message {
    /// Change the search term. If no prior search term is given this
    /// will start a search. If an existing search term is given this will
    /// stop the prior search and start a new one.
    ChangeNeedle(WriteReq),

    /// Select a search result.
    Select(Select),
}

/// Events that can be emitted from the search thread. The caller
/// chooses to handle these as they see fit.
#[derive(Debug, Clone, Copy)]
pub enum Event<'a> {
    /// Search is quitting. The search thread is exiting.
    Quit,

    /// Search is complete for the given needle on all screens.
    Complete,

    /// Total matches on the current active screen have changed.
    TotalMatches(usize),

    /// Selected match changed.
    SelectedMatch(Option<EventSelectedMatch<'a>>),

    /// Matches in the viewport have changed. The memory is owned by the
    /// search thread and is only valid during the callback.
    ViewportMatches(&'a [FlattenedHighlight]),
}

/// (Zig: `Event.SelectedMatch`.)
#[derive(Debug, Clone, Copy)]
pub struct EventSelectedMatch<'a> {
    pub idx: usize,
    pub highlight: &'a FlattenedHighlight,
}

/// The last-notified values used by `notify` to diff search state
/// into events.
#[derive(Debug, Clone, Copy)]
struct NotifyState {
    /// The active screen key the state below was captured against.
    key: ScreenKey,

    /// Last notified total matches count
    total: Option<usize>,

    /// Last notified selected match
    selected: Option<NotifySelected>,

    /// True if we sent the complete notification yet.
    complete: bool,
}

impl Default for NotifyState {
    fn default() -> Self {
        NotifyState {
            key: ScreenKey::Primary,
            total: None,
            selected: None,
            complete: false,
        }
    }
}

/// (Zig: `NotifyState.Selected`.)
#[derive(Debug, Clone, Copy)]
struct NotifySelected {
    idx: usize,
    highlight: UntrackedHighlight,
}

/// Errors from [`Thread::drain_mailbox`] (and the thread main loop).
#[derive(Debug)]
pub enum Error {
    OutOfMemory,
    Xev(xev::Error),
}

impl From<AllocError> for Error {
    fn from(_: AllocError) -> Self {
        Error::OutOfMemory
    }
}

impl From<xev::Error> for Error {
    fn from(err: xev::Error) -> Self {
        Error::Xev(err)
    }
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::OutOfMemory => f.write_str("OutOfMemory"),
            Error::Xev(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for Error {}

/// Holds the terminal mutex for its lifetime (Zig: `lockUncancelable` +
/// `defer unlock`).
struct LockGuard(NonNull<Mutex>);

impl LockGuard {
    fn new(mutex: NonNull<Mutex>) -> LockGuard {
        // SAFETY: the mutex outlives the thread (Options contract).
        unsafe { mutex.as_ref().lock_uncancelable(TinyIo) };
        LockGuard(mutex)
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        // SAFETY: as in `new`.
        unsafe { self.0.as_ref().unlock(TinyIo) };
    }
}

impl Thread {
    /// Initialize the thread. This does not START the thread. This only sets
    /// up all the internal state necessary prior to starting the thread. It
    /// is up to the caller to start the thread with the threadMain entrypoint.
    pub fn init(alloc: Allocator, opts: Options) -> Result<Thread, xev::Error> {
        // The mailbox for messaging this thread
        let mailbox = Mailbox::create();

        // Create our event loop.
        let r#loop = xev::Loop::init(xev::Options::default())?;

        // This async handle is used to "wake up" the renderer and force a render.
        let mut wakeup_h = xev::Async::init()?;

        // This async handle is used to stop the loop and force the thread to end.
        let mut stop_h = match xev::Async::init() {
            Ok(h) => h,
            Err(err) => {
                wakeup_h.deinit();
                return Err(err);
            }
        };

        // Refresh timer, see comments.
        let refresh_h = match xev::Timer::init() {
            Ok(h) => h,
            Err(err) => {
                stop_h.deinit();
                wakeup_h.deinit();
                return Err(err);
            }
        };

        Ok(Thread {
            alloc,
            mailbox,
            r#loop,
            wakeup: wakeup_h,
            wakeup_c: xev::Completion::default(),
            stop: stop_h,
            stop_c: xev::Completion::default(),
            refresh: refresh_h,
            refresh_c: xev::Completion::default(),
            refresh_active: false,
            search: None,
            notify_state: NotifyState::default(),
            opts,
        })
    }

    /// Clean up the thread. This is only safe to call once the thread
    /// completes executing; the caller must join prior to this.
    pub fn deinit(&mut self) {
        self.refresh.deinit();
        self.wakeup.deinit();
        self.stop.deinit();
        self.r#loop.deinit();
        // Nothing can possibly access the mailbox anymore; it is freed
        // with the thread.

        if let Some(mut s) = self.search.take() {
            let _lock = LockGuard::new(self.opts.mutex);
            // SAFETY: the terminal outlives the thread and we hold its lock.
            s.deinit(Some(unsafe { self.opts.terminal.as_ref() }));
        }
    }

    /// The main entrypoint for the thread.
    pub fn thread_main(&mut self) {
        let this: *mut Thread = self;
        // Call child function so we can use errors...
        // SAFETY: `this` is valid for the whole call.
        if let Err(err) = unsafe { Self::thread_main_(this) } {
            // In the future, we should expose this on the thread struct.
            log::warn!(target: LOG, "search thread err={err}");
        }
    }

    /// # Safety
    ///
    /// `this` and the pointers it holds must stay valid until this returns.
    unsafe fn thread_main_(this: *mut Thread) -> Result<(), Error> {
        struct ExitLog;
        impl Drop for ExitLog {
            fn drop(&mut self) {
                log::debug!(target: LOG, "search thread exited");
            }
        }
        let _exit_log = ExitLog;

        // Right now, on Darwin, `std.Thread.setName` can only name the current
        // thread, and we have no way to get the current thread from within it,
        // so instead we use this code to name the thread instead.
        #[cfg(target_os = "macos")]
        {
            // SAFETY: a valid NUL-terminated name.
            unsafe { crate::os::macos::pthread_setname_np(c"search".as_ptr()) };

            // We can run with lower priority than other threads.
            let class = crate::os::macos::QosClass::Utility;
            match crate::os::macos::set_qos_class(class) {
                Ok(()) => log::debug!(target: LOG, "thread QoS class set class={class:?}"),
                Err(err) => log::warn!(target: LOG, "error setting QoS class err={err:?}"),
            }
        }

        // Start the async handlers
        // SAFETY: the completions live in the thread, which outlives the
        // loop run (caller guarantee).
        unsafe {
            let t = &mut *this;
            t.wakeup.wait(&mut t.r#loop, &mut t.wakeup_c, this, wakeup_callback);
            t.stop.wait(&mut t.r#loop, &mut t.stop_c, this, stop_callback);
        }

        // Send an initial wakeup so we drain our mailbox immediately.
        // SAFETY: as above.
        unsafe { (*this).wakeup.notify()? };

        // Start the refresh timer
        // SAFETY: as above.
        unsafe { Self::start_refresh_timer(this) };

        // Run
        log::debug!(target: LOG, "starting search thread");
        struct Shutdown(*mut Thread);
        impl Drop for Shutdown {
            fn drop(&mut self) {
                log::debug!(target: LOG, "starting search thread shutdown");

                // Send the quit message
                // SAFETY: the thread is valid for the whole run.
                let opts = unsafe { (*self.0).opts };
                if let Some(cb) = opts.event_cb {
                    cb(Event::Quit, opts.event_userdata);
                }
            }
        }
        let _shutdown = Shutdown(this);

        // Unlike some of our other threads, we interleave search work
        // with our xev loop so that we can try to make forward search progress
        // while also listening for messages.
        loop {
            // SAFETY: the thread is valid for the whole run; callbacks run
            // only inside `loop.run` below and re-derive their own access.
            let t = unsafe { &mut *this };

            // If our loop is canceled then we drain our messages and quit.
            if t.r#loop.stopped() {
                while let Some(message) = t.mailbox.pop() {
                    log::debug!(target: LOG, "mailbox message ignored during shutdown={message:?}");
                }

                return Ok(());
            }

            let Some(s) = &mut t.search else {
                // If we're not actively searching, we can block the loop
                // until it does some work.
                // SAFETY: as above.
                unsafe { (*this).r#loop.run(xev::RunMode::Once)? };
                continue;
            };

            // If we have an active search, we always send any pending
            // notifications. Even if the search is complete, there may be
            // notifications to send.
            if let Some(cb) = t.opts.event_cb {
                notify(&mut t.notify_state, s, cb, t.opts.event_userdata);
            }

            if s.is_complete() {
                // If our search is complete, there's no more work to do, we
                // can block until we have an xev action.
                // SAFETY: as above.
                unsafe { (*this).r#loop.run(xev::RunMode::Once)? };
                continue;
            }

            // Tick the search. This will trigger any event callbacks, lock
            // for data loading, etc.
            match s.tick() {
                // We're complete now when we were not before. Notify!
                Tick::Complete => {}

                // Forward progress was made.
                Tick::Progress => {}

                // All searches are blocked. Let's grab the lock and feed data.
                Tick::Blocked => {
                    let _lock = LockGuard::new(t.opts.mutex);
                    feed_locked(t.opts.terminal, s);
                }
            }

            // We have an active search, so we only want to process messages
            // we have but otherwise return immediately so we can continue the
            // search. If the above completed the search, we still want to
            // go around the loop as quickly as possible to send notifications,
            // and then we'll block on the loop next time.
            // SAFETY: as above.
            unsafe { (*this).r#loop.run(xev::RunMode::NoWait)? };
        }
    }

    /// Drain the mailbox.
    fn drain_mailbox(&mut self) -> Result<(), Error> {
        while let Some(message) = self.mailbox.pop() {
            log::debug!(target: LOG, "mailbox message={message:?}");
            match message {
                Message::ChangeNeedle(v) => {
                    let r = self.change_needle(v.slice());
                    v.deinit();
                    r?;
                }
                Message::Select(v) => self.select(v)?,
            }
        }
        Ok(())
    }

    fn select(&mut self, sel: Select) -> Result<(), Error> {
        let Some(s) = &mut self.search else { return Ok(()) };

        let _lock = LockGuard::new(self.opts.mutex);

        // SAFETY: the terminal outlives the thread and we hold its lock.
        let t = unsafe { &mut *self.opts.terminal.as_ptr() };
        if s.select(t, sel, SelectScroll::IfNeeded)? {
            // No matter what we reset our selected match cache. This will
            // trigger a callback which will trigger the renderer to wake up
            // so it can be notified the screen scrolled.
            self.notify_state.selected = None;
        }

        Ok(())
    }

    /// Change the search term to the given value.
    fn change_needle(&mut self, needle: &[u8]) -> Result<(), Error> {
        log::debug!(target: LOG, "changing search needle to '{}'", String::from_utf8_lossy(needle));

        // Stop the previous search
        if let Some(s) = &mut self.search {
            // If our search is unchanged, do nothing.
            if s.needle().eq_ignore_ascii_case(needle) {
                return Ok(());
            }

            {
                let _lock = LockGuard::new(self.opts.mutex);
                // SAFETY: the terminal outlives the thread and we hold its lock.
                s.deinit(Some(unsafe { self.opts.terminal.as_ref() }));
            }
            self.search = None;
            self.notify_state = NotifyState::default();

            // When the search changes then we need to emit that it stopped.
            if let Some(cb) = self.opts.event_cb {
                cb(Event::TotalMatches(0), self.opts.event_userdata);
                cb(Event::SelectedMatch(None), self.opts.event_userdata);
                cb(Event::ViewportMatches(&[]), self.opts.event_userdata);
            }
        }

        // No needle means stop the search.
        if needle.is_empty() {
            return Ok(());
        }

        // Setup our search state.
        self.search = Some(TerminalSearch::init(self.alloc, needle)?);
        self.notify_state = NotifyState::default();

        // We need to grab the terminal lock and do an initial feed.
        let _lock = LockGuard::new(self.opts.mutex);
        feed_locked(self.opts.terminal, self.search.as_mut().unwrap());
        Ok(())
    }

    /// # Safety
    ///
    /// `this` must be valid and its completion must not move while armed.
    unsafe fn start_refresh_timer(this: *mut Thread) {
        // SAFETY: caller contract.
        let t = unsafe { &mut *this };

        // Set our active state so it knows we're running. We set this before
        // even checking the active state in case we have a pending shutdown.
        t.refresh_active = true;

        // If our timer is already active, then we don't have to do anything.
        if t.refresh_c.state() == xev::CompletionState::Active {
            return;
        }

        // Start the timer which loops
        // SAFETY: caller contract.
        unsafe {
            t.refresh.run(
                &mut t.r#loop,
                &mut t.refresh_c,
                REFRESH_INTERVAL,
                this,
                refresh_callback,
            );
        }
    }

    #[allow(dead_code)]
    fn stop_refresh_timer(&mut self) {
        // This will stop the refresh on the next iteration.
        self.refresh_active = false;
    }
}

/// Feed the search from the terminal state. The terminal mutex must
/// be held by the caller. (Zig: `Thread.feedLocked`.)
fn feed_locked(terminal: NonNull<Terminal>, s: &mut TerminalSearch) {
    // SAFETY: the terminal outlives the thread and the caller holds its lock.
    let t = unsafe { &mut *terminal.as_ptr() };

    // See the `search_viewport_dirty` flag on the terminal to know
    // what exactly this is for. But, if this is set, we know the renderer
    // found the viewport/active area dirty, so the active area must be
    // re-scanned.
    let active_dirty = t.flags.search_viewport_dirty;
    t.flags.search_viewport_dirty = false;

    s.feed(t, active_dirty);
}

/// Notify about any changes to the search state by diffing against
/// the last-notified values. (Zig: `Thread.notify`.)
///
/// This doesn't require any locking as it only reads search-owned state.
fn notify(state: &mut NotifyState, s: &mut TerminalSearch, cb: EventCallback, ud: *mut c_void) {
    // A screen switch makes all previously notified per-screen state
    // stale, so reset it to force recalculations and notifications.
    if state.key != s.active_key {
        state.key = s.active_key;
        state.total = None;
        state.selected = None;
    }

    let Some(screen_search) = s.active_screen_search() else { return };

    // Check our total match data
    let total = screen_search.matches_len();
    if Some(total) != state.total {
        log::debug!(target: LOG, "notifying total matches={total}");
        state.total = Some(total);
        cb(Event::TotalMatches(total), ud);
    }

    // Check our viewport matches. If they're stale, we collect them
    // now. We do this as part of notify and not tick because the
    // viewport search is very fast and doesn't require ticked progress
    // or feeds.
    if s.stale_viewport_matches {
        match s.viewport_matches() {
            Ok(matches) => {
                log::debug!(target: LOG, "notifying viewport matches len={}", matches.len());
                cb(Event::ViewportMatches(matches), ud);
            }
            Err(err) => log::warn!(target: LOG, "error collecting viewport matches err={err:?}"),
        }
    }

    // Check our last selected match data.
    let Some(screen_search) = s.active_screen_search() else { return };
    if let Some(m) = screen_search.selected {
        'match_: {
            let Some(flattened) = screen_search.selected_match() else { break 'match_ };
            let untracked = flattened.untracked();
            if let Some(prev) = state.selected {
                if prev.idx == m.idx && prev.highlight.eql(untracked) {
                    // Same selection, don't update it.
                    break 'match_;
                }
            }

            // New selection, notify!
            state.selected = Some(NotifySelected {
                idx: m.idx,
                highlight: untracked,
            });

            log::debug!(target: LOG, "notifying selection updated idx={}", m.idx);
            cb(
                Event::SelectedMatch(Some(EventSelectedMatch {
                    idx: m.idx,
                    highlight: flattened,
                })),
                ud,
            );
        }
    } else if state.selected.is_some() {
        log::debug!(target: LOG, "notifying selection cleared");
        state.selected = None;
        cb(Event::SelectedMatch(None), ud);
    }

    // Send our complete notification if we just completed.
    if !state.complete && s.is_complete() {
        log::debug!(target: LOG, "notifying search complete");
        state.complete = true;
        cb(Event::Complete, ud);
    }
}

fn wakeup_callback(
    self_: *mut Thread,
    _: &mut xev::Loop,
    _: &mut xev::Completion,
    r: Result<(), xev::r#async::WaitError>,
) -> xev::CallbackAction {
    if let Err(err) = r {
        log::warn!(target: LOG, "error in wakeup err={err}");
        return xev::CallbackAction::Rearm;
    }

    debug_assert!(!self_.is_null());

    // When we wake up, we drain the mailbox. Mailbox producers should
    // wake up our thread after publishing.
    // SAFETY: the thread outlives the loop run.
    if let Err(err) = unsafe { (*self_).drain_mailbox() } {
        log::warn!(target: LOG, "error draining mailbox err={err}");
    }

    xev::CallbackAction::Rearm
}

fn stop_callback(
    _self: *mut Thread,
    l: &mut xev::Loop,
    _: &mut xev::Completion,
    r: Result<(), xev::r#async::WaitError>,
) -> xev::CallbackAction {
    r.unwrap_or_else(|_| unreachable!());
    l.stop();
    xev::CallbackAction::Disarm
}

fn refresh_callback(
    self_: *mut Thread,
    l: &mut xev::Loop,
    c: &mut xev::Completion,
    r: Result<(), xev::timer::RunError>,
) -> xev::CallbackAction {
    r.unwrap_or_else(|_| unreachable!());
    if self_.is_null() {
        // This shouldn't happen so we log it.
        log::warn!(target: LOG, "refresh callback fired without data set");
        return xev::CallbackAction::Disarm;
    }
    // SAFETY: the thread outlives the loop run.
    let t = unsafe { &mut *self_ };

    // Run our feed if we have a search active.
    if let Some(s) = &mut t.search {
        let _lock = LockGuard::new(t.opts.mutex);
        feed_locked(t.opts.terminal, s);
    }

    // Only continue if we're still active
    if t.refresh_active {
        // SAFETY: the completion lives in the thread, which outlives the
        // loop run.
        unsafe { t.refresh.run(l, c, REFRESH_INTERVAL, self_, refresh_callback) };
    }

    xev::CallbackAction::Disarm
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datastruct::blocking_queue::Timeout;
    use crate::terminal::point::{Coordinate, Point, Tag};
    use crate::terminal::search::testing::{next_slice, terminal_default};
    use std::sync::{Condvar, Mutex as StdMutex};
    use std::time::Duration;

    #[derive(Default)]
    struct TestUserData {
        reset: (StdMutex<bool>, Condvar),
        total: StdMutex<usize>,
        selected: StdMutex<Option<(usize, FlattenedHighlight)>>,
        viewport: StdMutex<Vec<FlattenedHighlight>>,
    }

    impl TestUserData {
        fn callback(event: Event<'_>, userdata: *mut c_void) {
            // SAFETY: the test passes a live TestUserData.
            let ud = unsafe { &*(userdata as *const TestUserData) };
            match event {
                Event::Quit => {}
                Event::Complete => {
                    *ud.reset.0.lock().unwrap() = true;
                    ud.reset.1.notify_all();
                }
                Event::TotalMatches(v) => *ud.total.lock().unwrap() = v,
                Event::SelectedMatch(v) => {
                    *ud.selected.lock().unwrap() = v.map(|m| (m.idx, m.highlight.clone()));
                }
                Event::ViewportMatches(v) => *ud.viewport.lock().unwrap() = v.to_vec(),
            }
        }

        fn wait_timeout(&self, timeout: Duration) {
            let guard = self.reset.0.lock().unwrap();
            let (guard, res) = self.reset.1.wait_timeout_while(guard, timeout, |set| !*set).unwrap();
            assert!(*guard && !res.timed_out(), "timeout waiting for search completion");
        }
    }

    struct SendPtr(*mut Thread);
    // SAFETY: the test only touches thread-safe parts until it joins.
    unsafe impl Send for SendPtr {}

    #[test]
    fn thread() {
        let mut mutex = Mutex::new();
        let mut t = terminal_default(20, 2);
        next_slice(&mut t, b"Hello, world");

        let ud = TestUserData::default();
        let mut thread = Thread::init(
            Allocator::GLOBAL,
            Options {
                mutex: NonNull::from(&mut mutex),
                terminal: NonNull::from(&mut t),
                event_cb: Some(TestUserData::callback),
                event_userdata: &ud as *const TestUserData as *mut c_void,
            },
        )
        .unwrap();

        let mailbox: *const Mailbox = &*thread.mailbox;
        let wakeup = thread.wakeup;
        let stop = thread.stop;
        let ptr = SendPtr(&mut thread);
        let os_thread = std::thread::spawn(move || {
            let ptr = ptr;
            // SAFETY: the main thread waits for this thread to finish.
            unsafe { (*ptr.0).thread_main() };
        });

        // Start our search
        // SAFETY: the mailbox is thread-safe and outlives the thread run.
        let _ = unsafe { &*mailbox }.push(Message::ChangeNeedle(WriteReq::init(b"world")), Timeout::Forever);
        wakeup.notify().unwrap();

        // Wait for completion (the Zig test waits 100ms; allow more slack
        // for slow CI machines).
        ud.wait_timeout(Duration::from_secs(10));

        // Stop the thread
        stop.notify().unwrap();
        os_thread.join().unwrap();

        // 1 total matches
        assert_eq!(1, *ud.total.lock().unwrap());
        let viewport = ud.viewport.lock().unwrap();
        assert_eq!(1, viewport.len());
        {
            let sel = viewport[0].untracked();
            assert_eq!(
                Some(Point::Screen(Coordinate { x: 7, y: 0 })),
                t.screens.active.pages.point_from_pin(Tag::Screen, sel.start)
            );
            assert_eq!(
                Some(Point::Screen(Coordinate { x: 11, y: 0 })),
                t.screens.active.pages.point_from_pin(Tag::Screen, sel.end)
            );
        }
        drop(viewport);

        thread.deinit();
    }
}
