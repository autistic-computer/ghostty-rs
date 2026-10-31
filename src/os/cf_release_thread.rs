//! Port of `src/os/cf_release_thread.zig`.
//!
//! STATUS: ported (macOS-only; best effort, not compiled on Linux)
//!
//! Represents the CFRelease thread. Pools of CFTypeRefs are sent to
//! this thread to be released, so that their release callback logic
//! doesn't block the execution of a high throughput thread like the
//! renderer thread.
//!
//! Port notes:
//! * Like Zig's `*Thread`, the thread is shared with its spawned OS thread
//!   through a raw pointer ([`Thread::thread_main`]); other threads only
//!   touch `mailbox`, `wakeup` and `stop`, which are thread-safe.
//! * The `Release` message owns its list (Zig: slice + allocator).

#![cfg(target_os = "macos")]

use std::ffi::c_void;

use crate::datastruct::blocking_queue::BlockingQueue;
use crate::global::xev;
use crate::pkg::macos::foundation;

const LOG: &str = "cf_release_thread";

/// A CFTypeRef that may be sent to the release thread.
#[derive(Debug, Clone, Copy)]
pub struct CFTypeRef(pub *const c_void);
// SAFETY: CFRelease is thread-safe; ownership of the reference moves to
// the release thread.
unsafe impl Send for CFTypeRef {}

pub enum Message {
    /// Release a list of CFTypeRefs. The list is freed after releasing all
    /// the refs.
    Release { refs: Vec<CFTypeRef> },
}

/// The type used for sending messages to the thread. For now this is
/// hardcoded with a capacity. We can make this a comptime parameter in
/// the future if we want it configurable.
pub type Mailbox = BlockingQueue<Message, 64>;

#[derive(Debug, Default, Clone, Copy)]
struct Flags {
    /// This is set to true only when an abnormal exit is detected. It
    /// tells our mailbox system to drain and ignore all messages.
    drain: bool,
}

pub struct Thread {
    pub r#loop: xev::Loop,

    /// This can be used to wake up the thread.
    pub wakeup: xev::Async,
    pub wakeup_c: xev::Completion,

    /// This can be used to stop the thread on the next loop iteration.
    pub stop: xev::Async,
    pub stop_c: xev::Completion,

    /// The mailbox that can be used to send this thread messages. Note
    /// this is a blocking queue so if it is full you will get errors.
    pub mailbox: Box<Mailbox>,

    flags: Flags,
}

// SAFETY: like Zig, the loop and completions are only used by the release
// thread; other threads only use the thread-safe mailbox and asyncs.
unsafe impl Send for Thread {}
unsafe impl Sync for Thread {}

impl Thread {
    /// Initialize the thread. This does not START the thread. This only
    /// sets up all the internal state necessary prior to starting the
    /// thread. It is up to the caller to start the thread with the
    /// thread_main entrypoint.
    pub fn init() -> Result<Thread, xev::Error> {
        let mut r#loop = xev::Loop::init(xev::Options::default())?;

        let mut wakeup_h = match xev::Async::init() {
            Ok(v) => v,
            Err(err) => {
                r#loop.deinit();
                return Err(err);
            }
        };

        let stop_h = match xev::Async::init() {
            Ok(v) => v,
            Err(err) => {
                wakeup_h.deinit();
                r#loop.deinit();
                return Err(err);
            }
        };

        Ok(Thread {
            r#loop,
            wakeup: wakeup_h,
            wakeup_c: xev::Completion::default(),
            stop: stop_h,
            stop_c: xev::Completion::default(),
            mailbox: Box::new(Mailbox::new()),
            flags: Flags::default(),
        })
    }

    pub fn deinit(&mut self) {
        self.stop.deinit();
        self.wakeup.deinit();
        self.r#loop.deinit();
    }

    /// The main entrypoint for the thread.
    ///
    /// # Safety
    ///
    /// `this` must point to a thread that stays valid (and does not move)
    /// until this returns.
    pub unsafe fn thread_main(this: *mut Thread) {
        // SAFETY: caller guarantee.
        if let Err(err) = unsafe { Self::thread_main_(this) } {
            log::warn!(target: LOG, "error in cf release thread err={err}");
        }

        // If our loop is not stopped, then we need to keep running so that
        // messages are drained and we can wait for the surface to send a
        // stop message.
        // SAFETY: caller guarantee.
        if !unsafe { (*this).r#loop.stopped() } {
            log::warn!(
                target: LOG,
                "abrupt cf release thread exit detected, starting xev to drain mailbox"
            );
            // SAFETY: caller guarantee.
            unsafe { (*this).flags.drain = true };
            // SAFETY: caller guarantee.
            if let Err(err) = unsafe { (*this).r#loop.run(xev::RunMode::UntilDone) } {
                log::error!(target: LOG, "failed to start xev loop for draining err={err}");
            }
            log::debug!(target: LOG, "cf release thread fully exiting after abnormal failure");
        }
    }

    unsafe fn thread_main_(this: *mut Thread) -> Result<(), xev::Error> {
        // Right now, on Darwin, we can only name the current thread, so
        // we use this code to name the thread instead.
        // SAFETY: valid NUL-terminated name.
        unsafe { super::macos::pthread_setname_np(c"cf_release".as_ptr()) };

        // Start the async handlers. We start these first so that they're
        // registered even if anything below fails so we can drain the mailbox.
        // SAFETY: the completions live in the thread, which outlives the
        // loop run (caller guarantee).
        unsafe {
            let t = &mut *this;
            t.wakeup
                .wait(&mut t.r#loop, &mut t.wakeup_c, this, wakeup_callback);
            t.stop.wait(&mut t.r#loop, &mut t.stop_c, this, stop_callback);
        }

        // Run
        log::debug!(target: LOG, "starting cf release thread");
        // SAFETY: caller guarantee.
        let res = unsafe { (*this).r#loop.run(xev::RunMode::UntilDone) };
        log::debug!(target: LOG, "starting cf release thread shutdown");
        log::debug!(target: LOG, "cf release thread exited");
        res
    }

    /// Drain the mailbox, handling all the messages.
    ///
    /// # Safety
    ///
    /// `this` must be valid.
    unsafe fn drain_mailbox(this: *mut Thread) {
        // SAFETY: caller guarantee.
        let (drain, mailbox) = unsafe { ((*this).flags.drain, &*(*this).mailbox) };

        // If we're draining, we just drain the mailbox and return.
        if drain {
            while mailbox.pop().is_some() {}
            return;
        }

        while let Some(message) = mailbox.pop() {
            match message {
                Message::Release { refs } => {
                    for r in refs {
                        // SAFETY: the sender transferred ownership of one
                        // reference.
                        unsafe { foundation::cf_release(r.0 as *mut c_void) };
                    }
                }
            }
        }
    }
}

fn wakeup_callback(
    self_: *mut Thread,
    _: &mut xev::Loop,
    _: &mut xev::Completion,
    r: Result<(), xev::r#async::WaitError>,
) -> xev::CallbackAction {
    if let Err(err) = r {
        log::error!(target: LOG, "error in wakeup err={err}");
        return xev::CallbackAction::Rearm;
    }

    debug_assert!(!self_.is_null());

    // SAFETY: the thread outlives the loop run.
    unsafe { Thread::drain_mailbox(self_) };

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
