//! Port of `src/termio/mailbox.zig`.
//!
//! STATUS: ported
//!
//! Deviations: the queue is owned by the mailbox (`Box`, Zig: a pointer
//! freed in `deinit`); `std.Io` is [`TinyIo`] and the optional renderer
//! mutex is a [`crate::lib::tinyio::Mutex`]. A failed fast-path push hands
//! the message back (`BlockingQueue::push_or_return`) since Rust moves the
//! value into the queue.

use crate::datastruct::blocking_queue::Timeout;
use crate::datastruct::BlockingQueue;
use crate::global::xev;
use crate::lib::tinyio::{Mutex, TinyIo};
use crate::termio::Message;

/// A queue used for storing messages that is periodically drained.
/// Typically used by a multi-threaded application. The capacity is
/// hardcoded to a value that empirically has made sense for Ghostty usage
/// but I'm open to changing it with good arguments.
pub type Queue = BlockingQueue<Message, 64>;

/// The location to where write-related messages are sent.
pub enum Mailbox {
    // /// Write messages to an unbounded list backed by an allocator.
    // /// This is useful for single-threaded applications where you're not
    // /// afraid of running out of memory. You should be careful that you're
    // /// processing this in a timely manner though since some heavy workloads
    // /// will produce a LOT of messages.
    // ///
    // /// At the time of authoring this, the primary use case for this is
    // /// testing more than anything, but it probably will have a use case
    // /// in libghostty eventually.
    // unbounded: std.ArrayList(termio.Message),
    /// Write messages to a SPSC queue for multi-threaded applications.
    Spsc { queue: Box<Queue>, wakeup: xev::Async },
}

impl std::fmt::Debug for Mailbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Mailbox::Spsc { wakeup, .. } => f
                .debug_struct("Spsc")
                .field("wakeup", wakeup)
                .finish_non_exhaustive(),
        }
    }
}

impl Mailbox {
    /// Init the SPSC writer.
    pub fn init_spsc() -> Result<Mailbox, xev::Error> {
        let queue = Queue::create();
        let wakeup = xev::Async::init()?;
        Ok(Mailbox::Spsc { queue, wakeup })
    }

    pub fn deinit(&mut self) {
        match self {
            Mailbox::Spsc { queue, wakeup } => {
                while let Some(msg) = queue.pop() {
                    msg.deinit();
                }
                wakeup.deinit();
            }
        }
    }

    /// Sends the given message without notifying there are messages.
    ///
    /// If the optional mutex is given, it must already be LOCKED. If the
    /// send would block, we'll unlock this mutex, resend the message, and
    /// lock it again. This handles an edge case where queues are full.
    /// This may not apply to all writer types.
    pub fn send(&self, msg: Message, mutex: Option<&Mutex>) {
        match self {
            Mailbox::Spsc { queue, wakeup } => {
                // Try to write to the queue with an instant timeout. This is the
                // fast path because we can queue without a lock.
                let msg = match queue.push_or_return(msg, Timeout::Instant) {
                    Ok(_) => return,
                    Err(msg) => msg,
                };

                // If we enter this conditional, the queue is full. We wake up
                // the writer thread so that it can process messages to clear up
                // space. However, the writer thread may require the renderer
                // lock so we need to unlock.
                if let Err(err) = wakeup.notify() {
                    log::warn!(
                        target: "io_writer",
                        "failed to wake up writer, data will be dropped err={err}"
                    );
                    msg.deinit();
                    return;
                }

                // Unlock the renderer state so the writer thread can acquire it.
                // Then try to queue our message before continuing. This is a very
                // slow path because we are having a lot of contention for data.
                // But this only gets triggered in certain pathological cases.
                //
                // Note that writes themselves don't require a lock, but there
                // are other messages in the writer queue (resize, focus) that
                // could acquire the lock. This is why we have to release our lock
                // here.
                let io = TinyIo;
                if let Some(m) = mutex {
                    m.unlock(io);
                }
                if let Err(msg) = queue.push_or_return(msg, Timeout::Forever) {
                    msg.deinit();
                }
                if let Some(m) = mutex {
                    m.lock_uncancelable(io);
                }
            }
        }
    }

    /// Notify that there are new messages. This may be a noop depending
    /// on the writer type.
    pub fn notify(&self) {
        match self {
            Mailbox::Spsc { wakeup, .. } => {
                if let Err(err) = wakeup.notify() {
                    log::warn!(
                        target: "io_writer",
                        "failed to notify writer, data will be dropped err={err}"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spsc_send_and_drain() {
        let mut mb = Mailbox::init_spsc().unwrap();
        mb.send(Message::Focused(true), None);
        mb.send(Message::write_req(&[b'x'; 100]), None);
        mb.notify();
        match &mb {
            Mailbox::Spsc { queue, .. } => {
                assert!(matches!(queue.pop(), Some(Message::Focused(true))));
            }
        }
        // The remaining allocated write is freed by deinit.
        mb.deinit();
    }

    #[test]
    fn spsc_send_full_queue_with_mutex() {
        use std::sync::Arc;

        let mb = Arc::new(Mailbox::init_spsc().unwrap());
        for _ in 0..64 {
            mb.send(Message::Crash, None);
        }

        // The queue is full: the next send must unlock the mutex, block until
        // a consumer makes room, and relock.
        let mutex = Arc::new(Mutex::new());
        mutex.lock_uncancelable(TinyIo);
        let consumer = {
            let mb = Arc::clone(&mb);
            let mutex = Arc::clone(&mutex);
            std::thread::spawn(move || {
                // Wait for the producer to release the lock, then consume.
                mutex.lock_uncancelable(TinyIo);
                mutex.unlock(TinyIo);
                match &*mb {
                    Mailbox::Spsc { queue, .. } => {
                        assert!(queue.pop().is_some());
                    }
                }
            })
        };
        mb.send(Message::Focused(false), Some(&mutex));
        // We hold the lock again.
        assert!(!mutex.try_lock());
        mutex.unlock(TinyIo);
        consumer.join().unwrap();

        let mut mb = Arc::try_unwrap(mb).unwrap();
        mb.deinit();
    }
}
