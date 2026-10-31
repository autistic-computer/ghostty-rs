//! Port of libxev `src/watcher/stream.zig` (the `GenericStream` with its
//! `Readable` and `Writeable` parts, and the shared `WriteQueue` /
//! `WriteRequest` types).
//!
//! The `Closeable` and `Pollable` parts are not ported (Ghostty doesn't use
//! them).

use std::ffi::c_void;

use crate::pkg::xev::common::CallbackAction;
use crate::pkg::xev::epoll::{Completion, Loop, OpResult, Operation, ReadBuffer, WriteBuffer};
use crate::pkg::xev::posix::{Fd, ReadError, WriteError};

/// The typed callback of [`Stream::write`] / [`Stream::queue_write`].
pub type WriteCallback<U> = fn(
    ud: *mut U,
    l: &mut Loop,
    c: &mut Completion,
    s: Stream,
    b: WriteBuffer,
    r: Result<usize, WriteError>,
) -> CallbackAction;

/// The typed callback of [`Stream::read`].
pub type ReadCallback<U> = fn(
    ud: *mut U,
    l: &mut Loop,
    c: &mut Completion,
    s: Stream,
    b: ReadBuffer,
    r: Result<usize, ReadError>,
) -> CallbackAction;

/// WriteRequest is a single request for a write. It wraps a
/// completion so that it can be inserted into the WriteQueue.
#[repr(C)]
#[derive(Debug)]
pub struct WriteRequest {
    /// Must be the first field so that [`WriteRequest::from`] can recover
    /// the request from its completion.
    pub completion: Completion,
    pub userdata: *mut c_void,

    /// This is the original buffer passed to queueWrite. We have to keep
    /// track of this because we may be forced to split the write or rearm
    /// the write due to partial writes, but when we call the final callback
    /// we want to pass the original complete buffer.
    pub full_write_buffer: WriteBuffer,

    next: *mut WriteRequest,
}

// SAFETY: see `Completion`.
unsafe impl Send for WriteRequest {}

impl Default for WriteRequest {
    fn default() -> Self {
        Self {
            completion: Completion::default(),
            userdata: std::ptr::null_mut(),
            full_write_buffer: WriteBuffer::Array {
                array: [0; 32],
                len: 0,
            },
            next: std::ptr::null_mut(),
        }
    }
}

impl WriteRequest {
    /// This can be used to convert a completion pointer back to
    /// a WriteRequest. This is only safe of course if the completion
    /// originally is from a write request. This is useful for getting
    /// the WriteRequest back in a callback from queuedWrite.
    pub fn from(c: *mut Completion) -> *mut WriteRequest {
        // `completion` is the first field of a repr(C) struct.
        c as *mut WriteRequest
    }
}

/// WriteQueue is the queue of write requests for ordered writes.
/// This can be copied around (Zig: `queue.Intrusive(WriteRequest)`).
#[derive(Debug, Clone, Copy)]
pub struct WriteQueue {
    head: *mut WriteRequest,
    tail: *mut WriteRequest,
}

// SAFETY: see `Completion`.
unsafe impl Send for WriteQueue {}

impl Default for WriteQueue {
    fn default() -> Self {
        Self {
            head: std::ptr::null_mut(),
            tail: std::ptr::null_mut(),
        }
    }
}

impl WriteQueue {
    /// Enqueue a new element to the back of the queue.
    ///
    /// # Safety
    ///
    /// `v` must be valid and not already in a queue.
    unsafe fn push(&mut self, v: *mut WriteRequest) {
        // SAFETY: caller guarantees validity.
        unsafe {
            debug_assert!((*v).next.is_null());
            if self.tail.is_null() {
                self.head = v;
            } else {
                (*self.tail).next = v;
            }
            self.tail = v;
        }
    }

    /// Dequeue the next element from the queue.
    ///
    /// # Safety
    ///
    /// All queued requests must be valid.
    unsafe fn pop(&mut self) -> Option<*mut WriteRequest> {
        if self.head.is_null() {
            return None;
        }
        let next = self.head;
        // SAFETY: caller guarantees validity.
        unsafe {
            self.head = (*next).next;
            if self.head.is_null() {
                self.tail = std::ptr::null_mut();
            }
            (*next).next = std::ptr::null_mut();
        }
        Some(next)
    }

    /// Returns true if the queue is empty.
    pub fn empty(&self) -> bool {
        self.head.is_null()
    }

    /// The first request in the queue, if any.
    pub fn head(&self) -> Option<*mut WriteRequest> {
        (!self.head.is_null()).then_some(self.head)
    }
}

/// Creates a generic stream type that supports read and write. This
/// can be used for any file descriptor that would exhibit normal blocking
/// behavior on read/write. This should NOT be used for local files because
/// local files have some special properties.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stream {
    /// The underlying file
    pub fd: Fd,
}

/// The result of a write callback, extracted from the completion.
struct WriteResult {
    writer: Stream,
    buf: WriteBuffer,
    result: Result<usize, WriteError>,
}

/// Extracts the result from a completion for a write callback.
fn write_result(c: &Completion, r: OpResult) -> WriteResult {
    let Operation::Write { fd, buffer } = c.op else {
        unreachable!("write callback on a non-write completion")
    };
    let OpResult::Write(result) = r else {
        unreachable!("write callback with non-write result")
    };
    WriteResult {
        writer: Stream::init_fd(fd),
        buf: buffer,
        result,
    }
}

/// Given a `WriteBuffer` and number of bytes written during the previous
/// write operation, returns a new `WriteBuffer` with remaining data.
fn write_buffer_remainder(buf: WriteBuffer, offset: usize) -> WriteBuffer {
    match buf {
        WriteBuffer::Slice { ptr, len } => {
            assert!(offset <= len);
            WriteBuffer::Slice {
                // SAFETY: offset is within the buffer.
                ptr: unsafe { ptr.add(offset) },
                len: len - offset,
            }
        }
        WriteBuffer::Array { array, len } => {
            assert!(offset <= len);
            let rem_len = len - offset;
            let mut wb = [0u8; 32];
            wb[..rem_len].copy_from_slice(&array[offset..][..rem_len]);
            WriteBuffer::Array {
                array: wb,
                len: rem_len,
            }
        }
    }
}

fn write_callback<U>(
    ud: *mut c_void,
    l: &mut Loop,
    c: &mut Completion,
    r: OpResult,
) -> CallbackAction {
    // SAFETY: `user_callback` was set from a `WriteCallback<U>` by
    // `Stream::write`, which also installed this wrapper.
    let cb: WriteCallback<U> =
        unsafe { std::mem::transmute::<*const (), WriteCallback<U>>(c.user_callback) };
    let cb_res = write_result(c, r);
    cb(ud as *mut U, l, c, cb_res.writer, cb_res.buf, cb_res.result)
}

fn queue_write_callback<U>(
    ud: *mut c_void,
    l: &mut Loop,
    c: &mut Completion,
    r: OpResult,
) -> CallbackAction {
    let user_cb = c.user_callback;
    // SAFETY: `user_callback` was set from a `WriteCallback<U>` by
    // `Stream::queue_write`, which also installed this wrapper.
    let cb: WriteCallback<U> =
        unsafe { std::mem::transmute::<*const (), WriteCallback<U>>(user_cb) };

    // SAFETY: the completion userdata is the queue (set by queue_write),
    // which the caller keeps valid while writes are queued.
    let q_inner = unsafe { &mut *(ud as *mut WriteQueue) };

    // The queue MUST have a request because a completion
    // can only be added if the queue is not empty, and
    // nothing else should be popping!.
    //
    // We only peek the request here (not pop) because we may
    // need to rearm this write if the write was partial.
    let req_inner: *mut WriteRequest = q_inner.head;
    debug_assert!(std::ptr::eq(WriteRequest::from(c), req_inner));

    let cb_res = write_result(c, r);
    let mut result = cb_res.result;

    // Checks whether the entire buffer was written, this is
    // necessary to guarantee correct ordering of writes.
    // If the write was partial, it re-submits the remainder of
    // the buffer.
    let queued_len = cb_res.buf.len();
    if let Ok(written_len) = cb_res.result {
        if written_len < queued_len {
            // Write remainder of the buffer, reusing the same completion
            // (`c` is `req_inner.completion`).
            let rem_buf = write_buffer_remainder(cb_res.buf, written_len);
            cb_res.writer.write_init(c, rem_buf);
            c.userdata = q_inner as *mut WriteQueue as *mut c_void;
            c.callback = queue_write_callback::<U>;
            c.user_callback = user_cb;
            // SAFETY: the request is still queued, so the caller keeps it
            // valid.
            unsafe { l.add(c) };
            return CallbackAction::Disarm;
        }

        // We wrote the entire buffer, modify the result to indicate
        // to the caller that all bytes have been written.
        // SAFETY: req_inner is valid; this field is disjoint from `c`.
        result = Ok(unsafe { (*req_inner).full_write_buffer.len() });
    }

    // We can pop previously peeked request.
    // SAFETY: queued requests are valid.
    let popped = unsafe { q_inner.pop() };
    debug_assert_eq!(popped, Some(req_inner));

    // SAFETY: req_inner is valid; these fields are disjoint from `c`.
    let (req_ud, full) = unsafe { ((*req_inner).userdata, (*req_inner).full_write_buffer) };
    let action = cb(req_ud as *mut U, l, c, cb_res.writer, full, result);

    // Rearm requeues this request, it doesn't return rearm
    // on the actual callback here...
    if action == CallbackAction::Rearm {
        // Unlike the Zig original, re-initialize the completion with the
        // full buffer: the loop closes this completion's dup'd fd once we
        // return, and a partial write may have shrunk the buffer.
        cb_res.writer.write_init(c, full);
        c.userdata = q_inner as *mut WriteQueue as *mut c_void;
        c.callback = queue_write_callback::<U>;
        c.user_callback = user_cb;
        // SAFETY: the caller asked to requeue, so the request stays valid.
        unsafe { q_inner.push(req_inner) };
    }

    // If we have another request, add that completion next.
    if let Some(req_next) = q_inner.head() {
        // SAFETY: queued requests are valid.
        unsafe { l.add(std::ptr::addr_of_mut!((*req_next).completion)) };
    }

    // We always disarm because the completion in the next
    // request will be used if there is more to queue.
    CallbackAction::Disarm
}

fn read_callback<U>(
    ud: *mut c_void,
    l: &mut Loop,
    c: &mut Completion,
    r: OpResult,
) -> CallbackAction {
    // SAFETY: `user_callback` was set from a `ReadCallback<U>` by
    // `Stream::read`, which also installed this wrapper.
    let cb: ReadCallback<U> =
        unsafe { std::mem::transmute::<*const (), ReadCallback<U>>(c.user_callback) };
    let Operation::Read { fd, buffer } = c.op else {
        unreachable!("read callback on a non-read completion")
    };
    let OpResult::Read(r) = r else {
        unreachable!("read callback with non-read result")
    };
    cb(ud as *mut U, l, c, Stream::init_fd(fd), buffer, r)
}

impl Stream {
    /// Initialize a generic stream from a file descriptor.
    pub fn init_fd(fd: Fd) -> Stream {
        Stream { fd }
    }

    /// Clean up any watcher resources. This does NOT close the file.
    /// If you want to close the file you must call close or do so
    /// synchronously.
    pub fn deinit(&self) {}

    /// Read from the stream.
    ///
    /// # Safety
    ///
    /// `c` (and a [`ReadBuffer::Slice`] buffer) must not move and must stay
    /// valid until the completion is dead again, and `userdata` must stay
    /// valid for the callback (see [`crate::pkg::xev::epoll`]).
    pub unsafe fn read<U>(
        &self,
        l: &mut Loop,
        c: &mut Completion,
        buf: ReadBuffer,
        userdata: *mut U,
        cb: ReadCallback<U>,
    ) {
        *c = Completion::new(
            Operation::Read {
                fd: self.fd,
                buffer: buf,
            },
            userdata as *mut c_void,
            read_callback::<U>,
        );
        c.user_callback = cb as *const ();

        // If we're dup-ing, then we ask the backend to manage the fd.
        c.set_dup(true);

        // SAFETY: forwarded caller guarantee.
        unsafe { l.add(c) };
    }

    /// Write to the stream. This queues the writes to ensure they
    /// remain in order. Queueing has a small overhead: you must
    /// maintain a WriteQueue and WriteRequests instead of just
    /// Completions.
    ///
    /// If ordering isn't important, or you can maintain ordering
    /// naturally in your program, consider using write since it
    /// has a slightly smaller overhead.
    ///
    /// The "CallbackAction" return value of this callback behaves slightly
    /// different. The "rearm" return value will re-queue the same write
    /// at the end of the queue.
    ///
    /// It is safe to call this at anytime from the main thread.
    ///
    /// # Safety
    ///
    /// `q` and `req` (and a [`WriteBuffer::Slice`] buffer) must not move
    /// and must stay valid until the request's callback has run, and
    /// `userdata` must stay valid for the callback (see
    /// [`crate::pkg::xev::epoll`]).
    pub unsafe fn queue_write<U>(
        &self,
        l: &mut Loop,
        q: &mut WriteQueue,
        req: &mut WriteRequest,
        buf: WriteBuffer,
        userdata: *mut U,
        cb: WriteCallback<U>,
    ) {
        // Initialize our completion
        *req = WriteRequest {
            full_write_buffer: buf,
            ..Default::default()
        };
        // Must be kept in sync with partial write logic inside the callback
        self.write_init(&mut req.completion, buf);
        req.completion.userdata = q as *mut WriteQueue as *mut c_void;
        req.completion.callback = queue_write_callback::<U>;
        req.completion.user_callback = cb as *const ();

        // The userdata as to go on the WriteRequest because we need
        // our actual completion userdata to be the WriteQueue so that
        // we can process the queue.
        req.userdata = userdata as *mut c_void;

        // If the queue is empty, then we add our completion. Otherwise,
        // the previously queued writes will trigger this one.
        if q.empty() {
            // SAFETY: forwarded caller guarantee.
            unsafe { l.add(&mut req.completion) };
        }

        // We always add this item to our queue no matter what
        // SAFETY: forwarded caller guarantee.
        unsafe { q.push(req) };
    }

    /// Write to the stream. This performs a single write. Additional
    /// writes can be requested by calling this multiple times.
    ///
    /// IMPORTANT: writes are NOT queued. There is no order guarantee
    /// if this is called multiple times. If ordered writes are important
    /// (they usually are!) then you should only call write again once
    /// the previous write callback is called.
    ///
    /// If ordering is important, use queueWrite instead.
    ///
    /// # Safety
    ///
    /// Same as [`Stream::read`].
    pub unsafe fn write<U>(
        &self,
        l: &mut Loop,
        c: &mut Completion,
        buf: WriteBuffer,
        userdata: *mut U,
        cb: WriteCallback<U>,
    ) {
        self.write_init(c, buf);
        c.userdata = userdata as *mut c_void;
        c.callback = write_callback::<U>;
        c.user_callback = cb as *const ();

        // SAFETY: forwarded caller guarantee.
        unsafe { l.add(c) };
    }

    /// Initialize the completion c for a write. This does NOT set
    /// userdata or a callback.
    pub fn write_init(&self, c: &mut Completion, buf: WriteBuffer) {
        *c = Completion::new(
            Operation::Write {
                fd: self.fd,
                buffer: buf,
            },
            std::ptr::null_mut(),
            crate::pkg::xev::common::noop_callback,
        );

        // If we're dup-ing, then we ask the backend to manage the fd.
        c.set_dup(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pkg::xev::common::{CompletionState, Options, RunMode};
    use crate::pkg::xev::posix;

    /// Helper to open a pty. This isn't exposed as a public API this is only
    /// used for tests.
    struct Pty {
        /// The file descriptors for the parent/child side of the pty. This refers
        /// to the master/slave side respectively, and while that terminology is
        /// the officially used terminology of the syscall, I will use parent/child
        /// here.
        parent: Fd,
        child: Fd,
    }

    impl Pty {
        fn init() -> Pty {
            // Reasonable size
            let mut size = libc::winsize {
                ws_row: 80,
                ws_col: 80,
                ws_xpixel: 800,
                ws_ypixel: 600,
            };
            let mut parent: Fd = -1;
            let mut child: Fd = -1;
            // SAFETY: valid out pointers.
            let rc = unsafe {
                libc::openpty(
                    &mut parent,
                    &mut child,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut size as *mut libc::winsize as _,
                )
            };
            assert!(rc >= 0, "openpty failed");
            Pty { parent, child }
        }
    }

    impl Drop for Pty {
        fn drop(&mut self) {
            posix::close(self.parent);
            posix::close(self.child);
        }
    }

    fn record_read(
        ud: *mut Option<usize>,
        _: &mut Loop,
        _: &mut Completion,
        _: Stream,
        _: ReadBuffer,
        r: Result<usize, ReadError>,
    ) -> CallbackAction {
        // SAFETY: points to the live `read_len`.
        unsafe { *ud = Some(r.unwrap()) };
        CallbackAction::Disarm
    }

    fn ignore_write(
        _: *mut (),
        _: &mut Loop,
        _: &mut Completion,
        _: Stream,
        _: WriteBuffer,
        r: Result<usize, WriteError>,
    ) -> CallbackAction {
        r.unwrap();
        CallbackAction::Disarm
    }

    #[test]
    fn pty_child_to_parent() {
        // Create the pty parent/child side.
        let pty = Pty::init();
        let mut l = Loop::init(Options::default()).unwrap();

        let parent = Stream::init_fd(pty.parent);
        let child = Stream::init_fd(pty.child);

        // Read
        let mut read_buf = [0u8; 128];
        let mut read_len: Option<usize> = None;
        let mut c_read = Completion::default();
        unsafe {
            parent.read(
                &mut l,
                &mut c_read,
                ReadBuffer::slice(&mut read_buf),
                &mut read_len,
                record_read,
            )
        };

        // This should not block!
        l.run(RunMode::NoWait).unwrap();
        assert!(read_len.is_none());

        // Send
        let send_buf = b"hello, world!";
        let mut c_write = Completion::default();
        unsafe {
            child.write(
                &mut l,
                &mut c_write,
                WriteBuffer::slice(send_buf),
                std::ptr::null_mut(),
                ignore_write,
            )
        };

        // The write and read should trigger
        l.run(RunMode::UntilDone).unwrap();
        let n = read_len.unwrap();
        assert_eq!(&read_buf[..n], send_buf);
    }

    #[test]
    fn pty_parent_to_child() {
        // Create the pty parent/child side.
        let pty = Pty::init();
        let mut l = Loop::init(Options::default()).unwrap();

        let parent = Stream::init_fd(pty.parent);
        let child = Stream::init_fd(pty.child);

        // Read
        let mut read_buf = [0u8; 128];
        let mut read_len: Option<usize> = None;
        let mut c_read = Completion::default();
        unsafe {
            child.read(
                &mut l,
                &mut c_read,
                ReadBuffer::slice(&mut read_buf),
                &mut read_len,
                record_read,
            )
        };

        // This should not block!
        l.run(RunMode::NoWait).unwrap();
        assert!(read_len.is_none());

        // Send (note the newline at the end of the buf is important
        // since we're in cooked mode)
        let send_buf = b"hello, world!\n";
        let mut c_write = Completion::default();
        unsafe {
            parent.write(
                &mut l,
                &mut c_write,
                WriteBuffer::slice(send_buf),
                std::ptr::null_mut(),
                ignore_write,
            )
        };

        // The write and read should trigger
        l.run(RunMode::UntilDone).unwrap();
        let n = read_len.unwrap();
        assert_eq!(&read_buf[..n], send_buf);
    }

    #[test]
    fn pty_queued_writes() {
        // Create the pty parent/child side.
        let pty = Pty::init();
        let mut l = Loop::init(Options::default()).unwrap();

        let parent = Stream::init_fd(pty.parent);
        let child = Stream::init_fd(pty.child);

        // Read
        let mut read_buf = [0u8; 128];
        let mut read_len: Option<usize> = None;
        let mut c_read = Completion::default();
        unsafe {
            child.read(
                &mut l,
                &mut c_read,
                ReadBuffer::slice(&mut read_buf),
                &mut read_len,
                record_read,
            )
        };

        // This should not block!
        l.run(RunMode::NoWait).unwrap();
        assert!(read_len.is_none());

        let mut write_queue = WriteQueue::default();
        let mut write_req: [WriteRequest; 2] = Default::default();

        // Send (note the newline at the end of the buf is important
        // since we're in cooked mode)
        let (req0, req1) = write_req.split_at_mut(1);
        unsafe {
            parent.queue_write(
                &mut l,
                &mut write_queue,
                &mut req0[0],
                WriteBuffer::slice(b"hello, "),
                std::ptr::null_mut(),
                ignore_write,
            )
        };

        let mut c_result: *mut Completion = std::ptr::null_mut();
        unsafe {
            parent.queue_write(
                &mut l,
                &mut write_queue,
                &mut req1[0],
                WriteBuffer::slice(b"world!\n"),
                &mut c_result,
                |ud: *mut *mut Completion, _, c, _, _, r| {
                    r.unwrap();
                    // SAFETY: points to the live `c_result`.
                    *ud = c;
                    CallbackAction::Disarm
                },
            )
        };

        // The write and read should trigger
        l.run(RunMode::UntilDone).unwrap();
        let n = read_len.unwrap();
        assert_eq!(&read_buf[..n], b"hello, world!\n");

        // Verify our completion is equal to our request
        assert!(std::ptr::eq(
            WriteRequest::from(c_result),
            &write_req[1] as *const WriteRequest
        ));
        assert!(write_queue.empty());
        assert_eq!(write_req[0].completion.state(), CompletionState::Dead);
        assert_eq!(write_req[1].completion.state(), CompletionState::Dead);
    }

    #[test]
    fn queued_writes_preserve_order() {
        // Many queued writes through a pipe arrive in order, and the
        // reported length is that of each full request.
        let mut fds = [0; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let (r, w) = (fds[0], fds[1]);

        let mut l = Loop::init(Options::default()).unwrap();
        let stream = Stream::init_fd(w);
        let mut q = WriteQueue::default();
        let chunks: Vec<Vec<u8>> = (0..20u8)
            .map(|i| vec![b'a' + i; 100 + i as usize])
            .collect();
        let mut reqs: Vec<WriteRequest> =
            (0..chunks.len()).map(|_| WriteRequest::default()).collect();
        let mut lens: Vec<usize> = Vec::new();
        for (req, chunk) in reqs.iter_mut().zip(chunks.iter()) {
            unsafe {
                stream.queue_write(
                    &mut l,
                    &mut q,
                    req,
                    WriteBuffer::slice(chunk),
                    &mut lens,
                    |ud: *mut Vec<usize>, _, _, _, _, r| {
                        // SAFETY: points to the live `lens`.
                        (*ud).push(r.unwrap());
                        CallbackAction::Disarm
                    },
                )
            };
        }
        l.run(RunMode::UntilDone).unwrap();
        assert_eq!(lens, chunks.iter().map(Vec::len).collect::<Vec<_>>());

        posix::close(w);
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = posix::read(r, &mut buf).unwrap();
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
        assert_eq!(out, chunks.concat());
        posix::close(r);
    }

    #[test]
    fn write_buffer_remainder_array() {
        let mut array = [0u8; 32];
        array[..5].copy_from_slice(b"hello");
        let rem = write_buffer_remainder(WriteBuffer::Array { array, len: 5 }, 2);
        assert_eq!(unsafe { rem.as_slice() }, b"llo");
        let rem = write_buffer_remainder(WriteBuffer::slice(b"hello"), 5);
        assert!(rem.is_empty());
    }
}
