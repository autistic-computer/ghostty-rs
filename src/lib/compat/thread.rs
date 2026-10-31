//! Port of `src/lib/compat/thread.zig`.
//!
//! STATUS: ported
//!
//! Code taken from Zig 0.17.0 `std.Io.Condition`. See README.md for license
//! and details. Zig 0.16's `std.Io.Condition` lacks a timed wait; Rust's
//! `std::sync::Condvar` has one, so the port maps the Zig signature onto
//! `Condvar::wait_timeout`, keeping the semantics: return when signaled,
//! or fail with `Timeout` once the deadline passes with no signal.

use std::fmt;
use std::sync::{Condvar, MutexGuard};
use std::time::{Duration, Instant};

/// Zig: `std.Io.Timeout`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timeout {
    /// Wait forever.
    None,
    /// Wait at most this long.
    Duration(Duration),
    /// Wait until this instant.
    Deadline(Instant),
}

impl Timeout {
    fn to_deadline(self) -> Option<Instant> {
        match self {
            Timeout::None => None,
            Timeout::Duration(d) => Some(Instant::now() + d),
            Timeout::Deadline(d) => Some(d),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitTimeoutError {
    Timeout,
}

impl fmt::Display for WaitTimeoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Timeout")
    }
}

impl std::error::Error for WaitTimeoutError {}

/// Blocks until the condition is signaled or the provided timeout expires.
/// The mutex guard is released while waiting and re-acquired before
/// returning (in both the success and the timeout case), so it is always
/// returned to the caller.
///
/// Like any condition variable, spurious wakeups are possible: callers
/// must re-check their predicate.
pub fn wait_timeout<'a, T>(
    cond: &Condvar,
    guard: MutexGuard<'a, T>,
    timeout: Timeout,
) -> (MutexGuard<'a, T>, Result<(), WaitTimeoutError>) {
    let deadline = timeout.to_deadline();
    match deadline {
        None => {
            let guard = cond.wait(guard).unwrap_or_else(|e| e.into_inner());
            (guard, Ok(()))
        }
        Some(deadline) => {
            let now = Instant::now();
            if deadline <= now {
                return (guard, Err(WaitTimeoutError::Timeout));
            }
            let (guard, result) = cond
                .wait_timeout(guard, deadline - now)
                .unwrap_or_else(|e| e.into_inner());
            if result.timed_out() {
                (guard, Err(WaitTimeoutError::Timeout))
            } else {
                (guard, Ok(()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn wait_timeout_times_out() {
        let m = Mutex::new(());
        let c = Condvar::new();
        let g = m.lock().unwrap();
        let (_g, r) = wait_timeout(&c, g, Timeout::Duration(Duration::from_millis(5)));
        assert_eq!(Err(WaitTimeoutError::Timeout), r);
    }

    #[test]
    fn wait_timeout_signaled() {
        let pair = Arc::new((Mutex::new(false), Condvar::new()));
        let pair2 = pair.clone();
        let t = std::thread::spawn(move || {
            let (m, c) = &*pair2;
            *m.lock().unwrap() = true;
            c.notify_all();
        });
        let (m, c) = &*pair;
        let mut g = m.lock().unwrap();
        while !*g {
            let (g2, r) = wait_timeout(c, g, Timeout::Duration(Duration::from_secs(10)));
            g = g2;
            assert!(r.is_ok() || *g);
        }
        drop(g);
        t.join().unwrap();
    }
}
