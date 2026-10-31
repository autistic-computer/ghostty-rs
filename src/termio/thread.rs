//! Port of `src/termio/Thread.zig`.
//!
//! STATUS: ported
//!
//! Represents the "writer" thread for terminal IO. The reader side is
//! handled by the Termio struct itself and dependent on the underlying
//! implementation (i.e. if its a pty, manual, etc.).
//!
//! The writer thread does handle writing bytes to the pty but also handles
//! different events such as starting synchronized output, changing some
//! modes (like linefeed), etc. The goal is to offload as much from the
//! reader thread as possible since it is the hot path in parsing VT
//! sequences and updating terminal state.
//!
//! This thread state can only be used by one thread at a time.
//!
//! Deviations from the Zig code:
//! * The allocator field is dropped.
//! * The callback data is created by [`Thread::thread_main`] (Zig: by
//!   `threadMain_`) so that it outlives the drain loop that runs after an
//!   abnormal exit; that loop still has the wakeup and stop completions
//!   armed with it.
//! * The xev callbacks reach the loop through their `&mut Loop` argument
//!   rather than through `Thread.loop`, since the loop is borrowed by its
//!   own `run` while they execute.
//! * The error messages written to the terminal on an abnormal exit are
//!   built by [`Thread::error_message`].

use std::fmt;

use crate::crash;
use crate::global::xev;
use crate::lib::tinyio::TinyIo;
use crate::terminal::csi::EraseDisplay;
use crate::terminal::modes::Mode;
use crate::renderer;
use crate::termio::exec;
use crate::terminal::kitty::clipboard_grants::Direction as KittyClipboardDirection;
use crate::termio::termio::{crash_surface, Error, Termio, ThreadData as TermioThreadData};
use crate::termio::{Mailbox, Message};

const LOG: &str = "io_thread";

/// This stores the information that is coalesced.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Coalesce {
    pub resize: Option<renderer::Size>,
}

impl Coalesce {
    /// The number of milliseconds to coalesce certain messages like resize for.
    /// Not all message types are coalesced.
    pub const MIN_MS: u64 = 25;
}

/// The number of milliseconds before we reset the synchronized output flag
/// if the running program hasn't already.
const SYNC_RESET_MS: u64 = 1000;

/// The number of milliseconds between each movement during selection scrolling.
const SELECTION_SCROLL_MS: u64 = 15;

/// Thread flags (Zig: a packed struct).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Flags {
    /// This is set to true only when an abnormal exit is detected. It
    /// tells our mailbox system to drain and ignore all messages.
    pub drain: bool,

    /// True if linefeed mode is enabled. This is duplicated here so that the
    /// write thread doesn't need to grab a lock to check this on every write.
    pub linefeed_mode: bool,

    /// This is true when the inspector is active.
    pub has_inspector: bool,
}

/// The termio thread. Once [`Thread::thread_main`] runs, the thread must
/// not move: completions armed on its loop point into it.
pub struct Thread {
    /// The main event loop for the thread.
    pub r#loop: xev::Loop,

    /// The completion to use for the wakeup async handle that is present
    /// on the termio.Writer.
    pub wakeup_c: xev::Completion,

    /// This can be used to stop the thread on the next loop iteration.
    pub stop: xev::Async,
    pub stop_c: xev::Completion,

    /// This is used for timer-based selection scrolling.
    pub scroll: xev::Timer,
    pub scroll_c: xev::Completion,
    pub scroll_active: bool,

    /// This is used to coalesce resize events.
    pub coalesce: xev::Timer,
    pub coalesce_c: xev::Completion,
    pub coalesce_cancel_c: xev::Completion,
    pub coalesce_data: Coalesce,

    /// This timer is used to reset synchronized output modes so that
    /// the terminal doesn't freeze with a bad actor.
    pub sync_reset: xev::Timer,
    pub sync_reset_c: xev::Completion,
    pub sync_reset_cancel_c: xev::Completion,

    pub flags: Flags,
}

impl fmt::Debug for Thread {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Thread")
            .field("stop", &self.stop)
            .field("scroll_active", &self.scroll_active)
            .field("coalesce_data", &self.coalesce_data)
            .field("flags", &self.flags)
            .finish_non_exhaustive()
    }
}

/// This is the data passed to xev callbacks on the thread.
struct CallbackData {
    self_: *mut Thread,
    io: *mut Termio,
    data: TermioThreadData,
}

impl Thread {
    /// Initialize the thread. This does not START the thread. This only sets
    /// up all the internal state necessary prior to starting the thread. It
    /// is up to the caller to start the thread with the threadMain entrypoint.
    pub fn init() -> Result<Thread, xev::Error> {
        // Create our event loop.
        let mut r#loop = xev::Loop::init(xev::Options::default())?;

        // This async handle is used to stop the loop and force the thread to end.
        let mut stop_h = match xev::Async::init() {
            Ok(h) => h,
            Err(err) => {
                r#loop.deinit();
                return Err(err);
            }
        };

        // This timer is used for selection scrolling.
        // This timer is used to coalesce resize events.
        // This timer is used to reset synchronized output modes.
        let timers = (|| Ok((xev::Timer::init()?, xev::Timer::init()?, xev::Timer::init()?)))();
        let (scroll_h, coalesce_h, sync_reset_h) = match timers {
            Ok(t) => t,
            Err(err) => {
                stop_h.deinit();
                r#loop.deinit();
                return Err(err);
            }
        };

        Ok(Thread {
            r#loop,
            wakeup_c: xev::Completion::default(),
            stop: stop_h,
            stop_c: xev::Completion::default(),
            scroll: scroll_h,
            scroll_c: xev::Completion::default(),
            scroll_active: false,
            coalesce: coalesce_h,
            coalesce_c: xev::Completion::default(),
            coalesce_cancel_c: xev::Completion::default(),
            coalesce_data: Coalesce::default(),
            sync_reset: sync_reset_h,
            sync_reset_c: xev::Completion::default(),
            sync_reset_cancel_c: xev::Completion::default(),
            flags: Flags::default(),
        })
    }

    /// Clean up the thread. This is only safe to call once the thread
    /// completes executing; the caller must join prior to this.
    pub fn deinit(&mut self) {
        self.scroll.deinit();
        self.coalesce.deinit();
        self.sync_reset.deinit();
        self.stop.deinit();
        self.r#loop.deinit();
    }

    /// The main entrypoint for the thread.
    ///
    /// `io` must stay valid until this returns. Other threads may only use
    /// copies of the `stop` handle and the termio mailbox while this runs.
    pub fn thread_main(&mut self, io: *mut Termio) {
        let this: *mut Thread = self;

        // This is the data sent to xev callbacks. We want a pointer to both
        // ourselves and the thread data so we can thread that through (pun intended).
        let mut cb = CallbackData {
            self_: this,
            io,
            data: TermioThreadData::default(),
        };

        // Call child function so we can use errors...
        // SAFETY: `this` is valid for the whole call and `cb` outlives every
        // completion armed with it (both loop runs happen in this frame).
        if let Err(err) = unsafe { Self::thread_main_(this, io, &mut cb) } {
            log::warn!(target: LOG, "error in io thread err={err}");

            // If there is an error, we replace our terminal screen with
            // the error message. It might be better in the future to send
            // the error to the surface thread and let the apprt deal with it
            // in some way but this works for now. Without this, the user would
            // just see a blank terminal window.
            // SAFETY: `io` and the render state it points to are valid for
            // the whole call; we hold the renderer state mutex while
            // touching the terminal.
            unsafe {
                let state = &*(*io).renderer_state;
                let mutex = state.mutex.as_ref();
                mutex.lock_uncancelable(TinyIo);
                let t = &mut *state.terminal.as_ptr();

                // Hide the cursor
                t.modes.set(Mode::CursorVisible, false);

                let str = Self::error_message(err);
                t.erase_display(EraseDisplay::Complete, false);
                let _ = t.print_string(str.as_str());
                mutex.unlock(TinyIo);
            }
        }

        // If our loop is not stopped, then we need to keep running so that
        // messages are drained and we can wait for the surface to send a stop
        // message.
        // SAFETY: as above.
        let l = unsafe { &mut (*this).r#loop };
        if !l.stopped() {
            log::warn!(target: LOG, "abrupt io thread exit detected, starting xev to drain mailbox");
            // SAFETY: as above.
            unsafe { (*this).flags.drain = true };
            if let Err(err) = l.run(xev::RunMode::UntilDone) {
                log::error!(target: LOG, "failed to start xev loop for draining err={err:?}");
            }
            log::debug!(target: LOG, "io thread fully exiting after abnormal failure");
        }
    }

    /// The message Zig writes to the terminal when the IO thread fails.
    pub fn error_message(err: Error) -> String {
        match err {
            Error::Exec(exec::Error::OpenptyFailed) => "\
Your system cannot allocate any more pty devices.

Ghostty requires a pty device to launch a new terminal.
This error is usually due to having too many terminal
windows open or having another program that is using too
many pty devices.

Please free up some pty devices and try again."
                .to_string(),

            Error::InputNotFound | Error::InputFailed => "\
A configured `input` path was not found, was not readable,
was too large, or the underlying pty failed to accept
the write.

Ghostty can't continue since it can't guarantee that
initial terminal state will be as desired. Please review
the value of `input` in your configuration file and
ensure that all the path values exist and are readable."
                .to_string(),

            err => format!(
                "\
error starting IO thread: {err}

The underlying shell or command was unable to be started.
This error is usually due to exhausting a system resource.
If this looks like a bug, please report it.

This terminal is non-functional. Please close it and try again."
            ),
        }
    }

    /// # Safety
    ///
    /// `this`, `io` and `cb` must stay valid until this returns.
    unsafe fn thread_main_(
        this: *mut Thread,
        io: *mut Termio,
        cb: &mut CallbackData,
    ) -> Result<(), Error> {
        // Setup our crash metadata
        // SAFETY: caller guarantee.
        crash::sentry::set_thread_state(Some(crash::sentry::ThreadState {
            r#type: crash::sentry::ThreadType::Io,
            surface: crash_surface(unsafe { &(*io).surface_mailbox }),
        }));
        struct CrashGuard;
        impl Drop for CrashGuard {
            fn drop(&mut self) {
                crash::sentry::set_thread_state(None);
            }
        }
        let _crash_guard = CrashGuard;

        // Get the mailbox. This must be an SPSC mailbox for threading.
        // SAFETY: caller guarantee.
        let wakeup = match unsafe { &(*io).mailbox } {
            Mailbox::Spsc { wakeup, .. } => *wakeup,
        };

        // Run our thread start/end callbacks. This allows the implementation
        // to hook into the event loop as needed. The thread data is created
        // on the stack here so that it has a stable pointer throughout the
        // lifetime of the thread.
        // SAFETY: caller guarantee.
        unsafe { (*io).thread_enter(this, &mut cb.data)? };

        let cb_ptr: *mut CallbackData = cb;

        // Start the async handlers.
        // SAFETY: the completions live in the thread, and `cb` outlives the
        // loop runs (caller guarantee).
        unsafe {
            let t = &mut *this;
            wakeup.wait(&mut t.r#loop, &mut t.wakeup_c, cb_ptr, wakeup_callback);
            t.stop.wait(&mut t.r#loop, &mut t.stop_c, cb_ptr, stop_callback);
        }

        // Run
        log::debug!(target: LOG, "starting IO thread");
        // SAFETY: caller guarantee.
        let res = unsafe { (*this).r#loop.run(xev::RunMode::UntilDone) };
        log::debug!(target: LOG, "starting IO thread shutdown");

        // SAFETY: caller guarantee.
        unsafe {
            (*io).thread_exit(&mut (*cb_ptr).data);
            (*cb_ptr).data.deinit();
        }
        log::debug!(target: LOG, "IO thread exited");

        res?;
        Ok(())
    }

    /// Drain the mailbox, handling all the messages in our terminal implementation.
    ///
    /// # Safety
    ///
    /// Called from the wakeup callback with valid callback data.
    unsafe fn drain_mailbox(cb: *mut CallbackData, l: &mut xev::Loop) -> Result<(), Error> {
        // SAFETY: caller guarantee.
        let (this, io, data) = unsafe { ((*cb).self_, (*cb).io, &mut (*cb).data) };

        // We assert when starting the thread that this is the state
        // SAFETY: caller guarantee.
        let queue = match unsafe { &(*io).mailbox } {
            Mailbox::Spsc { queue, .. } => &**queue as *const crate::termio::mailbox::Queue,
        };
        // SAFETY: the queue is owned by the termio mailbox and thread-safe.
        let mailbox = unsafe { &*queue };

        // If we're draining, we just drain the mailbox and return.
        // SAFETY: caller guarantee.
        if unsafe { (*this).flags.drain } {
            while let Some(msg) = mailbox.pop() {
                msg.deinit();
            }
            return Ok(());
        }

        // SAFETY: caller guarantee (and see `Termio`).
        let io = unsafe { &mut *io };

        // This holds the mailbox lock for the duration of the drain. The
        // expectation is that all our message handlers will be non-blocking
        // ENOUGH to not mess up throughput on producers.
        let mut redraw = false;
        while let Some(message) = mailbox.pop() {
            // If we have a message we always redraw
            redraw = true;

            log::debug!(target: LOG, "mailbox message={}", message_tag(&message));
            // SAFETY: caller guarantee.
            let flags = unsafe { &mut (*this).flags };
            match message {
                Message::ColorSchemeReport { force } => io.color_scheme_report(data, force)?,
                Message::VisibilityReport { visible, force } => {
                    io.visibility_report(data, visible, force)?
                }
                Message::Crash => panic!("crash request, crashing intentionally"),
                Message::ChangeConfig(config) => io.change_config(data, *config)?,
                Message::Inspector(v) => flags.has_inspector = v,
                // SAFETY: caller guarantee.
                Message::Resize(v) => unsafe { Self::handle_resize(cb, l, v) },
                Message::SizeReport(v) => io.size_report(data, v)?,
                Message::ClearScreen { history } => io.clear_screen(data, history)?,
                Message::ScrollViewport(v) => io.scroll_viewport(v),
                Message::SelectionScroll(v) => {
                    if v {
                        // SAFETY: caller guarantee.
                        unsafe { Self::start_scroll_timer(cb, l) };
                    } else {
                        // SAFETY: caller guarantee.
                        unsafe { (*this).stop_scroll_timer() };
                    }
                }
                Message::JumpToPrompt(v) => io.jump_to_prompt(v)?,
                Message::KittyClipboardGrantRead(v) => {
                    io.kitty_clipboard_grant(&v.pw, KittyClipboardDirection::Read)?
                }
                Message::KittyClipboardGrantWrite(v) => {
                    io.kitty_clipboard_grant(&v.pw, KittyClipboardDirection::Write)?
                }
                // SAFETY: caller guarantee.
                Message::StartSynchronizedOutput => unsafe {
                    Self::start_synchronized_output(cb, l)
                },
                Message::LinefeedMode(v) => flags.linefeed_mode = v,
                Message::Focused(v) => io.focus_gained(data, v)?,
                Message::WriteSmall(v) => {
                    let linefeed = flags.linefeed_mode;
                    io.queue_write(data, &v.data[..v.len as usize], linefeed)?
                }
                Message::WriteStable(v) => {
                    let linefeed = flags.linefeed_mode;
                    io.queue_write(data, v, linefeed)?
                }
                Message::WriteAlloc(v) => {
                    let linefeed = flags.linefeed_mode;
                    io.queue_write(data, &v, linefeed)?
                }
            }
        }

        // Trigger a redraw after we've drained so we don't waste cyces
        // messaging a redraw.
        if redraw {
            io.renderer_wakeup.notify()?;
        }

        Ok(())
    }

    /// # Safety
    ///
    /// `cb` must be the thread's callback data.
    unsafe fn start_synchronized_output(cb: *mut CallbackData, l: &mut xev::Loop) {
        // SAFETY: caller guarantee; the completions live in the thread.
        unsafe {
            let this = &mut *(*cb).self_;
            this.sync_reset.reset(
                l,
                &mut this.sync_reset_c,
                &mut this.sync_reset_cancel_c,
                SYNC_RESET_MS,
                cb,
                sync_reset_callback,
            );
        }
    }

    /// # Safety
    ///
    /// `cb` must be the thread's callback data.
    unsafe fn handle_resize(cb: *mut CallbackData, l: &mut xev::Loop, resize: renderer::Size) {
        // SAFETY: caller guarantee.
        let this = unsafe { &mut *(*cb).self_ };
        this.coalesce_data.resize = Some(resize);

        // If the timer is already active we just return. In the future we want
        // to reset the timer up to a maximum wait time but for now this ensures
        // relatively smooth resizing.
        if this.coalesce_c.state() == xev::CompletionState::Active {
            return;
        }

        // SAFETY: the completions live in the thread.
        unsafe {
            this.coalesce.reset(
                l,
                &mut this.coalesce_c,
                &mut this.coalesce_cancel_c,
                Coalesce::MIN_MS,
                cb,
                coalesce_callback,
            )
        };
    }

    /// # Safety
    ///
    /// `cb` must be the thread's callback data.
    unsafe fn start_scroll_timer(cb: *mut CallbackData, l: &mut xev::Loop) {
        // SAFETY: caller guarantee.
        let this = unsafe { &mut *(*cb).self_ };
        this.scroll_active = true;

        match this.scroll_c.state() {
            // If it is already active, e.g. startScrollTimer is called multiple
            // times, then we just return. We can't simply check `scroll_active`
            // because its possible that `stopScrollTimer` was called but there
            // was no loop tick between then and now to halt out completion.
            xev::CompletionState::Active => {}

            // If the completion is not active then we need to start it.
            // SAFETY: the completion lives in the thread.
            xev::CompletionState::Dead => unsafe {
                this.scroll.run(
                    l,
                    &mut this.scroll_c,
                    SELECTION_SCROLL_MS,
                    cb,
                    selection_scroll_callback,
                )
            },
        }
    }

    fn stop_scroll_timer(&mut self) {
        // This will stop the scrolling on the next iteration.
        self.scroll_active = false;
    }
}

/// The Zig tag name of a message, for logging.
fn message_tag(message: &Message) -> &'static str {
    match message {
        Message::ColorSchemeReport { .. } => "color_scheme_report",
        Message::VisibilityReport { .. } => "visibility_report",
        Message::Crash => "crash",
        Message::ChangeConfig(_) => "change_config",
        Message::Inspector(_) => "inspector",
        Message::Resize(_) => "resize",
        Message::SizeReport(_) => "size_report",
        Message::ClearScreen { .. } => "clear_screen",
        Message::ScrollViewport(_) => "scroll_viewport",
        Message::SelectionScroll(_) => "selection_scroll",
        Message::JumpToPrompt(_) => "jump_to_prompt",
        Message::StartSynchronizedOutput => "start_synchronized_output",
        Message::LinefeedMode(_) => "linefeed_mode",
        Message::Focused(_) => "focused",
        Message::KittyClipboardGrantRead(_) => "kitty_clipboard_grant_read",
        Message::KittyClipboardGrantWrite(_) => "kitty_clipboard_grant_write",
        Message::WriteSmall(_) => "write_small",
        Message::WriteStable(_) => "write_stable",
        Message::WriteAlloc(_) => "write_alloc",
    }
}

fn sync_reset_callback(
    cb_: *mut CallbackData,
    _l: &mut xev::Loop,
    _c: &mut xev::Completion,
    r: Result<(), xev::timer::RunError>,
) -> xev::CallbackAction {
    match r {
        Ok(()) | Err(xev::timer::RunError::Canceled) => {}
        Err(err) => {
            log::warn!(target: LOG, "error during sync reset callback err={err}");
            return xev::CallbackAction::Disarm;
        }
    }

    if cb_.is_null() {
        return xev::CallbackAction::Disarm;
    }
    // SAFETY: the callback data outlives the loop.
    unsafe { (*(*cb_).io).reset_synchronized_output() };
    xev::CallbackAction::Disarm
}

fn coalesce_callback(
    cb_: *mut CallbackData,
    _l: &mut xev::Loop,
    _c: &mut xev::Completion,
    r: Result<(), xev::timer::RunError>,
) -> xev::CallbackAction {
    match r {
        Ok(()) | Err(xev::timer::RunError::Canceled) => {}
        Err(err) => {
            log::warn!(target: LOG, "error during coalesce callback err={err}");
            return xev::CallbackAction::Disarm;
        }
    }

    if cb_.is_null() {
        return xev::CallbackAction::Disarm;
    }
    // SAFETY: the callback data outlives the loop.
    let cb = unsafe { &mut *cb_ };

    // SAFETY: as above.
    let this = unsafe { &mut *cb.self_ };
    if let Some(v) = this.coalesce_data.resize.take() {
        // SAFETY: as above.
        if let Err(err) = unsafe { (*cb.io).resize(&mut cb.data, v) } {
            log::warn!(target: LOG, "error during resize err={err}");
        }
    }

    xev::CallbackAction::Disarm
}

fn wakeup_callback(
    cb_: *mut CallbackData,
    l: &mut xev::Loop,
    _c: &mut xev::Completion,
    r: Result<(), xev::r#async::WaitError>,
) -> xev::CallbackAction {
    if let Err(err) = r {
        log::error!(target: LOG, "error in wakeup err={err:?}");
        return xev::CallbackAction::Rearm;
    }

    // When we wake up, we check the mailbox. Mailbox producers should
    // wake up our thread after publishing.
    if cb_.is_null() {
        return xev::CallbackAction::Rearm;
    }
    // SAFETY: the callback data outlives the loop.
    if let Err(err) = unsafe { Thread::drain_mailbox(cb_, l) } {
        log::error!(target: LOG, "error draining mailbox err={err}");
    }

    xev::CallbackAction::Rearm
}

fn stop_callback(
    _cb: *mut CallbackData,
    l: &mut xev::Loop,
    _c: &mut xev::Completion,
    r: Result<(), xev::r#async::WaitError>,
) -> xev::CallbackAction {
    if r.is_err() {
        unreachable!();
    }
    l.stop();
    xev::CallbackAction::Disarm
}

fn selection_scroll_callback(
    cb_: *mut CallbackData,
    l: &mut xev::Loop,
    _c: &mut xev::Completion,
    r: Result<(), xev::timer::RunError>,
) -> xev::CallbackAction {
    match r {
        Ok(()) | Err(xev::timer::RunError::Canceled) => {}
        Err(err) => {
            log::warn!(target: LOG, "error during selection scroll callback err={err}");
            return xev::CallbackAction::Disarm;
        }
    }

    if cb_.is_null() {
        return xev::CallbackAction::Disarm;
    }
    // SAFETY: the callback data outlives the loop.
    let this = unsafe { &mut *(*cb_).self_ };

    // Send the tick to the main surface
    // SAFETY: as above.
    let _ = unsafe { &(*(*cb_).io).surface_mailbox }.push(
        crate::apprt::surface::Message::SelectionScrollTick(this.scroll_active),
        crate::datastruct::blocking_queue::Timeout::Instant,
    );

    if this.scroll_active {
        // SAFETY: the completion lives in the thread.
        unsafe {
            this.scroll.run(
                l,
                &mut this.scroll_c,
                SELECTION_SCROLL_MS,
                cb_,
                selection_scroll_callback,
            )
        };
    }

    xev::CallbackAction::Disarm
}

#[cfg(all(test, unix))]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::apprt::surface::Message as SurfaceMessage;
    use crate::termio::exec::testing::{direct_config, grid_size, Harness};
    use crate::termio::message::{KittyClipboardGrant, ScrollViewport, SizeReport};
    use crate::termio::DerivedConfig;

    fn size(cols: u32, rows: u32) -> renderer::Size {
        grid_size(cols, rows)
    }

    fn count(h: &Harness, pred: impl Fn(&SurfaceMessage) -> bool) -> usize {
        h.messages.iter().filter(|m| pred(m)).count()
    }

    // Extra (Thread.zig has no tests): resizes are coalesced and reach
    // the pty.
    #[test]
    fn resize_is_coalesced() {
        let mut h = Harness::new(direct_config(&[
            "/bin/sh",
            "-c",
            "echo ready; sleep 0.5; stty size; sleep 0.5",
        ]));
        h.start();
        h.wait_output("ready");
        h.send(Message::Resize(size(100, 40)));
        h.send(Message::Resize(size(90, 35)));
        h.send(Message::Resize(size(80, 30)));
        h.wait_io(|io| io.terminal.rows == 30);

        // Wait out a few more coalesce periods: nothing else arrives.
        std::thread::sleep(Duration::from_millis(Coalesce::MIN_MS * 4));
        h.collect_renderer_messages();
        let resizes: Vec<renderer::Size> = h
            .renderer_messages
            .iter()
            .filter_map(|m| match m {
                renderer::Message::Resize(s) => Some(*s),
                _ => None,
            })
            .collect();
        assert_eq!(vec![size(80, 30)], resizes);
        assert_eq!(size(80, 30), h.with_io(|io| io.size));
        assert_eq!((80, 30), h.with_io(|io| (io.terminal.cols, io.terminal.rows)));

        h.wait_output("30 80");
        h.stop();
    }

    // Extra: synchronized output is reset after the timeout.
    #[test]
    fn synchronized_output_reset() {
        let mut h = Harness::new(direct_config(&["/bin/sh", "-c", "sleep 30"]));
        h.start();
        h.with_io(|io| io.terminal.modes.set(Mode::SynchronizedOutput, true));
        let start = Instant::now();
        h.send(Message::StartSynchronizedOutput);
        h.wait_io(|io| !io.terminal.modes.get(Mode::SynchronizedOutput));
        assert!(start.elapsed() >= Duration::from_millis(SYNC_RESET_MS - 50));
        h.stop();
    }

    // Extra: selection scrolling ticks until stopped.
    #[test]
    fn selection_scroll_ticks() {
        let mut h = Harness::new(direct_config(&["/bin/sh", "-c", "sleep 30"]));
        h.start();
        h.send(Message::SelectionScroll(true));
        // Starting again while active is a no-op.
        h.send(Message::SelectionScroll(true));
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            h.collect_messages();
            if count(&h, |m| matches!(m, SurfaceMessage::SelectionScrollTick(true))) >= 3 {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }

        h.send(Message::SelectionScroll(false));
        h.wait_message(|m| matches!(m, SurfaceMessage::SelectionScrollTick(false)));

        // The timer stopped: no ticks after the final one.
        std::thread::sleep(Duration::from_millis(SELECTION_SCROLL_MS * 5));
        h.collect_messages();
        let last = h
            .messages
            .iter()
            .rposition(|m| matches!(m, SurfaceMessage::SelectionScrollTick(_)))
            .unwrap();
        assert!(matches!(h.messages[last], SurfaceMessage::SelectionScrollTick(false)));
        assert_eq!(
            1,
            count(&h, |m| matches!(m, SurfaceMessage::SelectionScrollTick(false)))
        );
        h.stop();
    }

    // Extra: losing focus pauses termios polling; regaining it checks
    // immediately.
    #[test]
    fn focus_pauses_termios_polling() {
        let mut h = Harness::new(direct_config(&[
            "/bin/sh",
            "-c",
            "echo ready; sleep 0.5; stty -echo; sleep 30",
        ]));
        h.start();
        h.wait_output("ready");
        h.send(Message::Focused(false));
        std::thread::sleep(Duration::from_millis(1200));
        h.collect_messages();
        assert_eq!(0, count(&h, |m| matches!(m, SurfaceMessage::PasswordInput(_))));

        h.send(Message::Focused(true));
        h.wait_message(|m| matches!(m, SurfaceMessage::PasswordInput(true)));
        h.stop();
    }

    // Extra: the remaining messages are handled and the flags they
    // control are updated.
    #[test]
    fn drain_mailbox_messages() {
        let mut h = Harness::new(direct_config(&[
            "/bin/sh",
            "-c",
            "read line; echo \"got:$line\"; sleep 0.5",
        ]));
        h.start();
        h.send(Message::ColorSchemeReport { force: true });
        h.send(Message::VisibilityReport {
            visible: true,
            force: false,
        });
        h.send(Message::ChangeConfig(Box::new(DerivedConfig::init(
            &crate::config::Config::default(),
        ))));
        h.send(Message::Inspector(true));
        h.send(Message::SizeReport(SizeReport::Mode2048));
        h.send(Message::ClearScreen { history: true });
        h.send(Message::ScrollViewport(ScrollViewport::Top));
        h.send(Message::JumpToPrompt(-1));
        h.send(Message::KittyClipboardGrantRead(KittyClipboardGrant {
            pw: b"pw".to_vec().into_boxed_slice(),
        }));
        h.send(Message::KittyClipboardGrantWrite(KittyClipboardGrant {
            pw: b"pw".to_vec().into_boxed_slice(),
        }));
        h.send(Message::WriteStable(b"stable "));
        h.send(Message::write_req(b"line\n"));
        h.wait_output("got:stable line");
        h.stop();
        assert!(h.thread.flags.has_inspector);
        assert!(!h.thread.flags.drain);
    }

    // Extra: a failing threadEnter makes the thread drain and exit.
    #[test]
    fn thread_enter_failure_drains() {
        let mut h = Harness::new(direct_config(&["/bin/s\0h"]));
        h.start();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !h.handle.as_ref().unwrap().is_finished() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        h.stop();
        assert!(h.thread.flags.drain);

        // The error is shown in the terminal.
        let screen = h.screen();
        assert!(screen.contains("error starting IO thread"), "{screen:?}");
        assert!(!h.with_io(|io| io.terminal.modes.get(Mode::CursorVisible)));
    }

    #[test]
    fn error_messages() {
        assert!(Thread::error_message(Error::Exec(exec::Error::OpenptyFailed))
            .starts_with("Your system cannot allocate any more pty devices."));
        assert!(Thread::error_message(Error::InputFailed)
            .starts_with("A configured `input` path was not found"));
        assert!(Thread::error_message(Error::Exec(exec::Error::SystemResources))
            .starts_with("error starting IO thread: SystemResources\n"));
    }
}
