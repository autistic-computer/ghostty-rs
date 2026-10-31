//! Port of libxev `src/watcher/process.zig`.
//!
//! Process management, such as waiting for process exit.
//!
//! On Linux this uses a pidfd (libxev `ProcessPidFd`). When `pidfd_open(2)`
//! is unavailable (kernels before 5.3, or other Unix systems) the Rust port
//! falls back to polling `waitpid(pid, WNOHANG)` on a loop timer, which
//! needs no SIGCHLD handler.

use std::ffi::c_void;

use crate::pkg::xev::common::CallbackAction;
use crate::pkg::xev::epoll::{Completion, Loop, OpResult, Operation};
use crate::pkg::xev::poller::events;
use crate::pkg::xev::posix::{self, Error, Fd};

/// The error that can come in the wait callback (Zig: `PollError ||
/// error{InvalidChild}`).
pub type WaitError = posix::PollError;

/// The typed callback of [`Process::wait`].
pub type WaitCallback<U> =
    fn(ud: *mut U, l: &mut Loop, c: &mut Completion, r: Result<u32, WaitError>) -> CallbackAction;

/// How often the `waitpid` fallback polls for exit.
const POLL_INTERVAL_MS: u64 = 10;

/// A process watcher. Like the Zig struct this is a plain handle (`Copy`);
/// call [`Process::deinit`] exactly once to release it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Process {
    /// pidfd file descriptor, or -1 when using the `waitpid` fallback.
    pub fd: Fd,

    /// The pid to watch.
    pub pid: libc::pid_t,
}

impl Process {
    /// Create a new process watcher for the given pid.
    pub fn init(pid: libc::pid_t) -> Result<Process, Error> {
        Self::init_impl(pid, true)
    }

    fn init_impl(pid: libc::pid_t, allow_pidfd: bool) -> Result<Process, Error> {
        #[cfg(target_os = "linux")]
        if allow_pidfd {
            // Note: SOCK_NONBLOCK == PIDFD_NONBLOCK (== O_NONBLOCK).
            // SAFETY: plain syscall.
            let res = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, libc::O_NONBLOCK) };
            if res >= 0 {
                let fd = res as Fd;
                // pidfds are always CLOEXEC.
                return Ok(Process { fd, pid });
            }
            match posix::errno() {
                // Kernel too old (or seccomp-filtered): fall back to polling.
                libc::ENOSYS | libc::EPERM => {}
                libc::EINVAL => return Err(Error::InvalidArgument),
                libc::EMFILE => return Err(Error::ProcessFdQuotaExceeded),
                libc::ENFILE => return Err(Error::SystemFdQuotaExceeded),
                libc::ENODEV | libc::ENOMEM => return Err(Error::SystemResources),
                e => return Err(posix::unexpected_errno(e)),
            }
        }

        #[cfg(not(target_os = "linux"))]
        let _ = allow_pidfd;

        Ok(Process { fd: -1, pid })
    }

    /// Clean up the process watcher.
    pub fn deinit(&mut self) {
        if self.fd >= 0 {
            posix::close(self.fd);
            self.fd = -1;
        }
    }

    /// Wait for the process to exit. This will automatically call
    /// `waitpid` or equivalent and report the exit status.
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
        if self.fd >= 0 {
            *c = Completion::new(
                Operation::Poll {
                    fd: self.fd,
                    events: events::IN,
                },
                userdata as *mut c_void,
                pidfd_callback::<U>,
            );
            c.user_callback = cb as *const ();
            // SAFETY: forwarded caller guarantee.
            unsafe { l.add(c) };
        } else {
            // SAFETY: forwarded caller guarantee.
            unsafe {
                arm_poll_timer::<U>(l, c, self.pid, userdata as *mut c_void, cb as *const ())
            };
        }
    }
}

#[cfg(target_os = "linux")]
fn pidfd_callback<U>(
    ud: *mut c_void,
    l: &mut Loop,
    c: &mut Completion,
    r: OpResult,
) -> CallbackAction {
    // SAFETY: `user_callback` was set from a `WaitCallback<U>` by
    // `Process::wait`, which also installed this wrapper.
    let cb: WaitCallback<U> =
        unsafe { std::mem::transmute::<*const (), WaitCallback<U>>(c.user_callback) };
    let OpResult::Poll(r) = r else {
        unreachable!("process callback with non-poll result")
    };

    let arg: Result<u32, WaitError> = match r {
        // If our poll failed, report that error.
        Err(err) => Err(err),
        Ok(()) => {
            // We need to wait on the pidfd because it is noted as ready
            let Operation::Poll { fd, .. } = c.op else {
                unreachable!()
            };
            // SAFETY: zeroed siginfo_t is a valid out value.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            // SAFETY: valid out pointer.
            let res =
                unsafe { libc::waitid(libc::P_PIDFD, fd as libc::id_t, &mut info, libc::WEXITED) };
            if res == 0 {
                // SAFETY: waitid filled a SIGCHLD siginfo.
                Ok(unsafe { info.si_status() } as u32)
            } else {
                match posix::errno() {
                    libc::ECHILD => Err(Error::InvalidChild),

                    // The fd isn't ready to read, I guess?
                    libc::EAGAIN => return CallbackAction::Rearm,
                    err => {
                        log::warn!(target: "xev", "unexpected process wait errno={err}");
                        Err(Error::Unexpected)
                    }
                }
            }
        }
    };

    cb(ud as *mut U, l, c, arg)
}

#[cfg(not(target_os = "linux"))]
fn pidfd_callback<U>(
    _: *mut c_void,
    _: &mut Loop,
    _: &mut Completion,
    _: OpResult,
) -> CallbackAction {
    unreachable!("pidfd is only available on Linux")
}

/// Arm `c` as a timer that polls `waitpid` (fallback mode). The
/// completion's `userdata` stays the caller's; the pid is kept in the
/// completion's auxiliary slot.
///
/// # Safety
///
/// See [`Process::wait`].
unsafe fn arm_poll_timer<U>(
    l: &mut Loop,
    c: &mut Completion,
    pid: libc::pid_t,
    userdata: *mut c_void,
    user_cb: *const (),
) {
    // SAFETY: forwarded caller guarantee.
    unsafe { l.timer(c, POLL_INTERVAL_MS, userdata, poll_callback::<U>) };
    c.user_callback = user_cb;
    c.aux = pid as i64;
}

fn poll_callback<U>(
    ud: *mut c_void,
    l: &mut Loop,
    c: &mut Completion,
    r: OpResult,
) -> CallbackAction {
    // SAFETY: `user_callback` was set from a `WaitCallback<U>` by
    // `Process::wait`, which also installed this wrapper.
    let cb: WaitCallback<U> =
        unsafe { std::mem::transmute::<*const (), WaitCallback<U>>(c.user_callback) };
    let pid = c.aux as libc::pid_t;

    let arg: Result<u32, WaitError> = match r {
        OpResult::Timer(Ok(crate::pkg::xev::epoll::TimerTrigger::Cancel)) => {
            // Canceling the internal timer cancels the wait.
            Err(Error::Unexpected)
        }
        OpResult::Timer(Ok(_)) => {
            let mut status: libc::c_int = 0;
            let res = loop {
                // SAFETY: valid out pointer.
                let res = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
                if res < 0 && posix::errno() == libc::EINTR {
                    continue;
                }
                break res;
            };
            if res == 0 {
                // Still running: poll again later.
                let user_cb = c.user_callback;
                // SAFETY: the completion was armed by the caller of
                // `Process::wait` under the same contract.
                unsafe { arm_poll_timer::<U>(l, c, pid, ud, user_cb) };
                return CallbackAction::Disarm;
            } else if res > 0 {
                // Same value the pidfd path reports (siginfo si_status):
                // the exit code, or the signal number if killed.
                if libc::WIFEXITED(status) {
                    Ok(libc::WEXITSTATUS(status) as u32)
                } else if libc::WIFSIGNALED(status) {
                    Ok(libc::WTERMSIG(status) as u32)
                } else {
                    Ok(status as u32)
                }
            } else {
                match posix::errno() {
                    libc::ECHILD => Err(Error::InvalidChild),
                    err => {
                        log::warn!(target: "xev", "unexpected process wait errno={err}");
                        Err(Error::Unexpected)
                    }
                }
            }
        }
        OpResult::Timer(Err(_)) => Err(Error::Unexpected),
        _ => unreachable!("process poll callback with non-timer result"),
    };

    cb(ud as *mut U, l, c, arg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pkg::xev::common::{Options, RunMode};

    fn spawn(code: i32) -> libc::pid_t {
        let child = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("exit {code}"))
            .spawn()
            .unwrap();
        child.id() as libc::pid_t
    }

    fn reap(pid: libc::pid_t) {
        let mut status = 0;
        // SAFETY: valid out pointer.
        unsafe { libc::waitpid(pid, &mut status, 0) };
    }

    fn record(
        ud: *mut Option<u32>,
        _: &mut Loop,
        _: &mut Completion,
        r: Result<u32, WaitError>,
    ) -> CallbackAction {
        // SAFETY: points to the live `code`.
        unsafe { *ud = Some(r.unwrap()) };
        CallbackAction::Disarm
    }

    fn run_wait(pid: libc::pid_t, allow_pidfd: bool) -> Option<u32> {
        let mut l = Loop::init(Options::default()).unwrap();
        let mut p = Process::init_impl(pid, allow_pidfd).unwrap();
        if allow_pidfd && cfg!(target_os = "linux") {
            assert!(p.fd >= 0);
        }

        // Wait
        let mut code: Option<u32> = None;
        let mut c_wait = Completion::default();
        unsafe { p.wait(&mut l, &mut c_wait, &mut code, record) };

        // Wait for wake
        l.run(RunMode::UntilDone).unwrap();
        p.deinit();
        code
    }

    #[test]
    fn process_wait() {
        assert_eq!(run_wait(spawn(0), true), Some(0));
    }

    #[test]
    fn process_wait_with_non_zero_exit_code() {
        assert_eq!(run_wait(spawn(42), true), Some(42));
    }

    #[test]
    fn process_wait_on_a_process_that_already_exited() {
        let pid = spawn(0);

        let mut l = Loop::init(Options::default()).unwrap();
        let mut p = Process::init(pid).unwrap();

        reap(pid);

        // Wait
        let mut code: Option<u32> = None;
        let mut c_wait = Completion::default();
        unsafe {
            p.wait(
                &mut l,
                &mut c_wait,
                &mut code,
                |ud: *mut Option<u32>, _, _, r| {
                    // SAFETY: points to the live `code`.
                    *ud = Some(r.unwrap_or(0));
                    CallbackAction::Disarm
                },
            )
        };

        // Wait for wake
        l.run(RunMode::UntilDone).unwrap();
        assert_eq!(code, Some(0));
        p.deinit();
    }

    #[test]
    fn process_wait_fallback_polling() {
        assert_eq!(run_wait(spawn(0), false), Some(0));
        assert_eq!(run_wait(spawn(7), false), Some(7));
    }

    #[test]
    fn process_wait_fallback_already_reaped() {
        let pid = spawn(0);
        reap(pid);
        let mut l = Loop::init(Options::default()).unwrap();
        let mut p = Process::init_impl(pid, false).unwrap();
        let mut res: Option<Result<u32, WaitError>> = None;
        let mut c_wait = Completion::default();
        unsafe {
            p.wait(
                &mut l,
                &mut c_wait,
                &mut res,
                |ud: *mut Option<Result<u32, WaitError>>, _, _, r| {
                    // SAFETY: points to the live `res`.
                    *ud = Some(r);
                    CallbackAction::Disarm
                },
            )
        };
        l.run(RunMode::UntilDone).unwrap();
        assert_eq!(res, Some(Err(Error::InvalidChild)));
        p.deinit();
    }

    #[test]
    fn process_wait_signaled() {
        let child = std::process::Command::new("sleep")
            .arg("10")
            .spawn()
            .unwrap();
        let pid = child.id() as libc::pid_t;
        let mut l = Loop::init(Options::default()).unwrap();
        let mut p = Process::init(pid).unwrap();
        // SAFETY: plain syscall.
        unsafe { libc::kill(pid, libc::SIGKILL) };
        let mut code: Option<u32> = None;
        let mut c_wait = Completion::default();
        unsafe { p.wait(&mut l, &mut c_wait, &mut code, record) };
        l.run(RunMode::UntilDone).unwrap();
        assert_eq!(code, Some(libc::SIGKILL as u32));
        p.deinit();
    }
}
