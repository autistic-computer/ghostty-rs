//! Port of `src/datastruct/blocking_queue.zig`.
//!
//! STATUS: ported
//!
//! Blocking queue implementation aimed primarily for message passing
//! between threads.

use std::mem::MaybeUninit;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;

/// The type we use for queue size types. We can optimize this
/// in the future to be the correct bit-size for our preallocated
/// size for this queue.
pub type Size = u32;

/// Specifies the timeout for an operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timeout {
    /// Fail instantly (non-blocking).
    Instant,

    /// Run forever or until interrupted
    Forever,

    /// Nanoseconds
    Ns(u64),
}

/// The state protected by the big mutex.
struct State<T, const CAPACITY: usize> {
    /// Our data. The values are uninitialized until they are written.
    data: [MaybeUninit<T>; CAPACITY],

    /// The next location to write (next empty loc) and next location
    /// to read (next non-empty loc). The number of written elements.
    write: Size,
    read: Size,
    len: Size,

    /// Number of pushers waiting on `cond_not_full`.
    not_full_waiters: usize,
}

impl<T, const CAPACITY: usize> State<T, CAPACITY> {
    // The bounds of this queue. We recast this to Size so we can do math.
    const BOUNDS: Size = CAPACITY as Size;

    /// Returns true if the queue is full. This is not public because
    /// it requires the lock to be held.
    #[inline(always)]
    fn full(&self) -> bool {
        self.len == Self::BOUNDS
    }

    /// Read and account for the next value. Requires `len > 0`.
    #[inline(always)]
    fn take(&mut self) -> T {
        let n = self.read;
        self.read += 1;
        if self.read >= Self::BOUNDS {
            self.read -= Self::BOUNDS;
        }
        self.len -= 1;

        // SAFETY: slots in [read, read+len) (mod bounds) are initialized;
        // `n` was the first of them and is no longer counted after this.
        unsafe { self.data[n as usize].assume_init_read() }
    }
}

impl<T, const CAPACITY: usize> Drop for State<T, CAPACITY> {
    fn drop(&mut self) {
        while self.len > 0 {
            drop(self.take());
        }
    }
}

/// Returns a blocking queue implementation for type T.
///
/// This is tailor made for ghostty usage so it isn't meant to be maximally
/// generic, but I'm happy to make it more generic over time. Traits of this
/// queue that are specific to our usage:
///
///   - Fixed size. We expect our queue to quickly drain and also not be
///     too large so we prefer a fixed size queue for now.
///   - No blocking pop. We use an external event loop mechanism such as
///     eventfd to notify our waiter that there is no data available so
///     we don't need to implement a blocking pop.
///   - Drain function. Most queues usually pop one at a time. We have
///     a mechanism for draining since on every IO loop our TTY drains
///     the full queue so we can get rid of the overhead of a ton of
///     locks and bounds checking and do a one-time drain.
///
/// One key usage pattern is that our blocking queues are single producer
/// single consumer (SPSC). This should let us do some interesting optimizations
/// in the future. At the time of writing this, the blocking queue implementation
/// is purposely naive to build something quickly, but we should benchmark
/// and make this more optimized as necessary.
pub struct BlockingQueue<T, const CAPACITY: usize> {
    /// The big mutex that must be held to read/write.
    mutex: Mutex<State<T, CAPACITY>>,

    /// A CV for being notified when the queue is no longer full. This is
    /// used for writing. Note we DON'T have a CV for waiting on the
    /// queue not being EMPTY because we use external notifiers for that.
    cond_not_full: Condvar,
}

impl<T, const CAPACITY: usize> Default for BlockingQueue<T, CAPACITY> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const CAPACITY: usize> BlockingQueue<T, CAPACITY> {
    /// Create an empty queue in place.
    pub fn new() -> Self {
        const { assert!(CAPACITY <= Size::MAX as usize) };
        Self {
            mutex: Mutex::new(State {
                data: [const { MaybeUninit::uninit() }; CAPACITY],
                write: 0,
                read: 0,
                len: 0,
                not_full_waiters: 0,
            }),
            cond_not_full: Condvar::new(),
        }
    }

    /// Allocate the blocking queue on the heap.
    pub fn create() -> Box<Self> {
        Box::new(Self::new())
    }

    /// Free all the resources for this queue. This should only be
    /// called once all producers and consumers have quit.
    pub fn destroy(self: Box<Self>) {
        drop(self);
    }

    fn lock(&self) -> MutexGuard<'_, State<T, CAPACITY>> {
        self.mutex.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Push a value to the queue. This returns the total size of the
    /// queue (unread items) after the push. A return value of zero
    /// means that the push failed.
    pub fn push(&self, value: T, timeout: Timeout) -> Size {
        self.push_or_return(value, timeout).unwrap_or(0)
    }

    /// Like [`push`](Self::push), but a failed push hands the value back
    /// instead of dropping it (Zig callers keep their copy of the value and
    /// retry or free it).
    pub fn push_or_return(&self, value: T, timeout: Timeout) -> Result<Size, T> {
        let mut state = self.lock();

        // The
        if state.full() {
            match timeout {
                // If we're not waiting, then we failed to write.
                Timeout::Instant => return Err(value),

                Timeout::Forever => {
                    state.not_full_waiters += 1;
                    state = self
                        .cond_not_full
                        .wait(state)
                        .unwrap_or_else(|e| e.into_inner());
                    state.not_full_waiters -= 1;
                }

                Timeout::Ns(ns) => {
                    state.not_full_waiters += 1;
                    let (s, result) = self
                        .cond_not_full
                        .wait_timeout(state, Duration::from_nanos(ns))
                        .unwrap_or_else(|e| e.into_inner());
                    state = s;
                    state.not_full_waiters -= 1;
                    if result.timed_out() {
                        return Err(value);
                    }
                }
            }

            // If we're still full, then we failed to write. This can
            // happen in situations where we are interrupted.
            if state.full() {
                return Err(value);
            }
        }

        // Add our data and update our accounting
        let w = state.write as usize;
        state.data[w].write(value);
        state.write += 1;
        if state.write >= State::<T, CAPACITY>::BOUNDS {
            state.write -= State::<T, CAPACITY>::BOUNDS;
        }
        state.len += 1;

        Ok(state.len)
    }

    /// Pop a value from the queue without blocking.
    pub fn pop(&self) -> Option<T> {
        let mut state = self.lock();

        // If we're empty we have nothing
        if state.len == 0 {
            return None;
        }

        // Get the index we're going to read data from and do some
        // accounting.
        let value = state.take();

        // If we have consumers waiting on a full queue, notify.
        if state.not_full_waiters > 0 {
            self.cond_not_full.notify_one();
        }

        Some(value)
    }

    /// Pop all values from the queue. This will hold the big mutex
    /// until `deinit` is called on (or the drop of) the return value.
    /// This is used if you know you're going to "pop" and utilize all the
    /// values quickly to avoid many locks, bounds checks, and cv signals.
    pub fn drain(&self) -> DrainIterator<'_, T, CAPACITY> {
        DrainIterator {
            queue: self,
            state: Some(self.lock()),
        }
    }
}

/// Iterator returned by [`BlockingQueue::drain`]. Holds the queue lock.
pub struct DrainIterator<'a, T, const CAPACITY: usize> {
    queue: &'a BlockingQueue<T, CAPACITY>,
    state: Option<MutexGuard<'a, State<T, CAPACITY>>>,
}

impl<T, const CAPACITY: usize> DrainIterator<'_, T, CAPACITY> {
    /// Release the lock. Equivalent to dropping the iterator.
    pub fn deinit(self) {
        drop(self);
    }
}

impl<T, const CAPACITY: usize> Iterator for DrainIterator<'_, T, CAPACITY> {
    type Item = T;

    fn next(&mut self) -> Option<T> {
        let state = self.state.as_mut()?;
        if state.len == 0 {
            return None;
        }

        // Read and account
        Some(state.take())
    }
}

impl<T, const CAPACITY: usize> Drop for DrainIterator<'_, T, CAPACITY> {
    fn drop(&mut self) {
        if let Some(state) = self.state.take() {
            // If we have consumers waiting on a full queue, notify.
            if state.not_full_waiters > 0 {
                self.queue.cond_not_full.notify_one();
            }

            // Unlock
            drop(state);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_push_and_pop() {
        type Q = BlockingQueue<u64, 4>;
        let q = Q::create();

        // Should have no values
        assert!(q.pop().is_none());

        // Push until we're full
        assert_eq!(1 as Size, q.push(1, Timeout::Instant));
        assert_eq!(2 as Size, q.push(2, Timeout::Instant));
        assert_eq!(3 as Size, q.push(3, Timeout::Instant));
        assert_eq!(4 as Size, q.push(4, Timeout::Instant));
        assert_eq!(0 as Size, q.push(5, Timeout::Instant));

        // Pop!
        assert_eq!(q.pop().unwrap(), 1);
        assert_eq!(q.pop().unwrap(), 2);
        assert_eq!(q.pop().unwrap(), 3);
        assert_eq!(q.pop().unwrap(), 4);
        assert!(q.pop().is_none());

        // Drain does nothing
        let mut it = q.drain();
        assert!(it.next().is_none());
        it.deinit();

        // Verify we can still push
        assert_eq!(1 as Size, q.push(1, Timeout::Instant));
        q.destroy();
    }

    #[test]
    fn timed_push() {
        type Q = BlockingQueue<u64, 1>;
        let q = Q::create();

        // Push
        assert_eq!(1 as Size, q.push(1, Timeout::Instant));
        assert_eq!(0 as Size, q.push(2, Timeout::Instant));

        // Timed push should fail
        assert_eq!(0 as Size, q.push(2, Timeout::Ns(1000)));
    }
}
