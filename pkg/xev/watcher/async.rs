//! Port of libxev `src/watcher/async.zig`.
//!
//! "Wake up" an event loop from any thread using an async completion.
//!
//! Linux uses an eventfd (libxev `AsyncEventFd`, with the `waitPoll`
//! variant used by the io_uring/epoll backends). Other Unix systems use a
//! non-blocking self-pipe in its place (libxev uses mach ports on Darwin,
//! which the Rust port does not implement).

use std::ffi::c_void;

use crate::pkg::xev::common::CallbackAction;
use crate::pkg::xev::epoll::{Completion, Loop, OpResult, Operation};
use crate::pkg::xev::poller::events;
use crate::pkg::xev::posix::{self, Error, Fd};

/// The error that can come in the wait callback.
pub type WaitError = posix::ReadError;

/// The typed callback of [`Async::wait`].
pub type WaitCallback<U> =
    fn(ud: *mut U, l: &mut Loop, c: &mut Completion, r: Result<(), WaitError>) -> CallbackAction;

/// An async notifier. This is a plain handle (it is `Copy`, like the Zig
/// struct that is passed around by value); call [`Async::deinit`] exactly
/// once to release it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Async {
    /// eventfd file descriptor (the read end of the self-pipe on non-Linux).
    pub fd: Fd,

    /// The write end of the self-pipe (non-Linux only).
    #[cfg(not(target_os = "linux"))]
    pub write_fd: Fd,
}

/// Wrapper converting the raw loop result into the typed wait callback.
fn wait_callback<U>(
    ud: *mut c_void,
    l: &mut Loop,
    c: &mut Completion,
    r: OpResult,
) -> CallbackAction {
    // SAFETY: `user_callback` was set from a `WaitCallback<U>` by
    // `Async::wait`, which also installed this wrapper.
    let cb: WaitCallback<U> =
        unsafe { std::mem::transmute::<*const (), WaitCallback<U>>(c.user_callback) };
    let OpResult::Poll(r) = r else {
        unreachable!("async callback with non-poll result")
    };

    if r.is_ok() {
        // We need to read so that we can consume the
        // eventfd value. We only read 8 bytes because
        // we only write up to 8 bytes and we own the fd.
        // We ignore errors here because we expect the
        // read to succeed given we just polled it.
        if let Operation::Poll { fd, .. } = c.op {
            let mut buf = [0u8; 8];
            #[cfg(target_os = "linux")]
            let _ = posix::read(fd, &mut buf);
            // A pipe may hold several notifications; drain them all so
            // they coalesce like the eventfd counter does.
            #[cfg(not(target_os = "linux"))]
            while let Ok(n) = posix::read(fd, &mut buf) {
                if n == 0 {
                    break;
                }
            }
        }
    }
    // Otherwise we'll call the callback with the error.

    cb(ud as *mut U, l, c, r)
}

impl Async {
    /// Create a new async. An async can be assigned to exactly one loop
    /// to be woken up. The completion must be allocated in advance.
    pub fn init() -> Result<Async, Error> {
        #[cfg(target_os = "linux")]
        {
            // SAFETY: plain syscall.
            let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
            if fd < 0 {
                return Err(posix::unexpected_errno(posix::errno()));
            }
            Ok(Async { fd })
        }

        #[cfg(not(target_os = "linux"))]
        {
            let mut fds = [0 as Fd; 2];
            // SAFETY: valid out array.
            if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
                return Err(posix::unexpected_errno(posix::errno()));
            }
            for fd in fds {
                // SAFETY: plain fcntl calls on fds we own.
                unsafe {
                    libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
                    let fl = libc::fcntl(fd, libc::F_GETFL);
                    libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
                }
            }
            Ok(Async {
                fd: fds[0],
                write_fd: fds[1],
            })
        }
    }

    /// Clean up the async. This will forcibly deinitialize any resources
    /// and may result in erroneous wait callbacks to be fired.
    pub fn deinit(&mut self) {
        posix::close(self.fd);
        #[cfg(not(target_os = "linux"))]
        posix::close(self.write_fd);
    }

    /// Wait for a message on this async. Note that async messages may be
    /// coalesced (or they may not be) so you should not expect a 1:1 mapping
    /// between send and wait.
    ///
    /// Just like the rest of libxev, the wait must be re-queued if you want
    /// to continue to be notified of async events.
    ///
    /// You should NOT register an async with multiple loops (the same loop
    /// is fine -- but unnecessary). The behavior when waiting on multiple
    /// loops is undefined.
    ///
    /// # Safety
    ///
    /// `c` must not move and must stay valid until it is dead again, and
    /// `userdata` must stay valid for the callback (see
    /// [`crate::pkg::xev::epoll`]).
    pub unsafe fn wait<U>(
        &self,
        l: &mut Loop,
        c: &mut Completion,
        userdata: *mut U,
        cb: WaitCallback<U>,
    ) {
        *c = Completion::new(
            // We use a poll operation instead of a read operation
            // because in Kernel 6.15.4, read was regressed for
            // io_uring on eventfd/timerfd and would block forever.
            // However, poll works fine.
            Operation::Poll {
                fd: self.fd,
                events: events::IN,
            },
            userdata as *mut c_void,
            wait_callback::<U>,
        );
        c.user_callback = cb as *const ();
        // SAFETY: forwarded caller guarantee.
        unsafe { l.add(c) };
    }

    /// Notify a loop to wake up synchronously. This should never block forever
    /// (it will always EVENTUALLY succeed regardless of if the loop is currently
    /// ticking or not).
    ///
    /// This is safe to call from any thread.
    ///
    /// Internal details subject to change but if you're relying on these
    /// details then you may want to consider using a lower level interface
    /// using the loop directly:
    ///
    ///   - linux: eventfd is used. If the eventfd write would block
    ///     (EAGAIN) then we assume success because the eventfd is full.
    pub fn notify(&self) -> Result<(), Error> {
        // We want to just write "1" in the correct byte order as our host.
        let val = 1u64.to_ne_bytes();
        #[cfg(target_os = "linux")]
        let res = posix::write(self.fd, &val);
        #[cfg(not(target_os = "linux"))]
        let res = posix::write(self.write_fd, &val[..1]);
        match res {
            Ok(_) | Err(Error::WouldBlock) => Ok(()),
            Err(err) => Err(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pkg::xev::common::{Options, RunMode};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    #[test]
    fn r#async() {
        let mut l = Loop::init(Options::default()).unwrap();
        let mut notifier = Async::init().unwrap();

        // Wait
        let mut wake = false;
        let mut c_wait = Completion::default();
        unsafe {
            notifier.wait(&mut l, &mut c_wait, &mut wake, |ud: *mut bool, _, _, r| {
                r.unwrap();
                // SAFETY: points to the live `wake`.
                *ud = true;
                CallbackAction::Rearm
            })
        };

        // Send a notification
        notifier.notify().unwrap();

        // Wait for wake
        l.run(RunMode::Once).unwrap();
        assert!(wake);

        // Make sure it only triggers once
        wake = false;
        l.run(RunMode::NoWait).unwrap();
        assert!(!wake);

        notifier.deinit();
    }

    #[test]
    fn async_notify_first() {
        let mut l = Loop::init(Options::default()).unwrap();
        let mut notifier = Async::init().unwrap();

        // Send a notification
        notifier.notify().unwrap();

        // Wait
        let mut wake = false;
        let mut c_wait = Completion::default();
        unsafe {
            notifier.wait(&mut l, &mut c_wait, &mut wake, |ud: *mut bool, _, _, r| {
                r.unwrap();
                // SAFETY: points to the live `wake`.
                *ud = true;
                CallbackAction::Disarm
            })
        };

        // Wait for wake
        l.run(RunMode::UntilDone).unwrap();
        assert!(wake);
        notifier.deinit();
    }

    #[test]
    fn async_batches_multiple_notifications() {
        let mut l = Loop::init(Options::default()).unwrap();
        let mut notifier = Async::init().unwrap();

        // Send a notification many times
        for _ in 0..5 {
            notifier.notify().unwrap();
        }

        // Wait
        let mut count: u32 = 0;
        let mut c_wait = Completion::default();
        unsafe {
            notifier.wait(&mut l, &mut c_wait, &mut count, |ud: *mut u32, _, _, r| {
                r.unwrap();
                // SAFETY: points to the live `count`.
                *ud += 1;
                CallbackAction::Rearm
            })
        };

        // Send a notification
        notifier.notify().unwrap();

        // Wait for wake
        l.run(RunMode::Once).unwrap();
        for _ in 0..10 {
            l.run(RunMode::NoWait).unwrap();
        }
        assert_eq!(count, 1);
        notifier.deinit();
    }

    #[test]
    fn async_notify_from_other_thread() {
        // The loop thread blocks in run(.until_done) and is woken up by
        // notifications sent from other threads; a "stop" async ends it.
        struct State {
            wakeups: u32,
            target: u32,
            acks: Arc<AtomicU32>,
        }

        let mut l = Loop::init(Options::default()).unwrap();
        let mut wakeup = Async::init().unwrap();
        let mut stop = Async::init().unwrap();
        let acks = Arc::new(AtomicU32::new(0));
        let mut state = State {
            wakeups: 0,
            target: 3,
            acks: acks.clone(),
        };

        let mut c_wakeup = Completion::default();
        let mut c_stop = Completion::default();
        unsafe {
            wakeup.wait(
                &mut l,
                &mut c_wakeup,
                &mut state,
                |ud: *mut State, _, _, r| {
                    r.unwrap();
                    // SAFETY: points to the live state.
                    let s = &mut *ud;
                    s.wakeups += 1;
                    s.acks.fetch_add(1, Ordering::SeqCst);
                    CallbackAction::Rearm
                },
            );
            stop.wait(
                &mut l,
                &mut c_stop,
                std::ptr::null_mut::<()>(),
                |_, l, _, r| {
                    r.unwrap();
                    l.stop();
                    CallbackAction::Disarm
                },
            );
        }

        let sender = std::thread::spawn(move || {
            for i in 0..3 {
                wakeup.notify().unwrap();
                // Wait for the loop to see it so notifications don't coalesce.
                while acks.load(Ordering::SeqCst) <= i {
                    std::thread::yield_now();
                }
            }
            stop.notify().unwrap();
        });

        l.run(RunMode::UntilDone).unwrap();
        sender.join().unwrap();
        assert!(l.stopped());
        assert_eq!(state.wakeups, state.target);
        wakeup.deinit();
        stop.deinit();
    }
}
