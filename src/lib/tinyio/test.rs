//! Port of `src/lib/tinyio/test.zig`.
//!
//! STATUS: ported
//!
//! Tests for TinyIo that only touch its public surface. Platform-specific
//! tests live next to the platform arm they exercise. The Windows-only
//! branches of the Zig tests (NT pipes, `NtQueryInformationFile` on a
//! closed handle) live with the Windows arm; this file runs on POSIX.

#![cfg(all(test, unix))]

use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::*;

/// A temporary directory with an open fd (Zig: `testing.tmpDir`).
struct TmpDir {
    path: PathBuf,
    handle: std::fs::File,
}

impl TmpDir {
    fn new() -> TmpDir {
        let mut name = [0u8; 8];
        TinyIo::INIT.random_secure(&mut name).unwrap();
        let hex: String = name.iter().map(|b| format!("{b:02x}")).collect();
        let path = std::env::temp_dir().join(format!("ghostty-tinyio-{hex}"));
        std::fs::create_dir_all(&path).unwrap();
        let handle = std::fs::File::open(&path).unwrap();
        TmpDir { path, handle }
    }

    fn dir(&self) -> Dir {
        Dir {
            handle: self.handle.as_raw_fd(),
        }
    }

    fn write_file(&self, name: &str, data: &[u8]) {
        std::fs::write(self.path.join(name), data).unwrap();
    }
}

impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Reads until every buffer is full or the stream ends. POSIX readv fills
/// all buffers in one call; NT reads one buffer per call.
fn read_streaming_all(file: File, io: TinyIo, buffers: &mut [&mut [u8]]) -> Result<usize, Error> {
    assert!(buffers.len() <= 8);
    let mut total = 0usize;
    let mut idx = 0usize;
    let mut off = 0usize;
    while idx < buffers.len() {
        let mut list: Vec<&mut [u8]> = Vec::new();
        for (i, b) in buffers.iter_mut().enumerate().skip(idx) {
            if i == idx {
                list.push(&mut b[off..]);
            } else {
                list.push(&mut b[..]);
            }
        }
        let mut n = match file.read_streaming(io, &mut list) {
            Ok(n) => n,
            Err(Error::EndOfStream) => break,
            Err(e) => return Err(e),
        };
        if n == 0 {
            break;
        }
        total += n;
        while idx < buffers.len() && n >= buffers[idx].len() - off {
            n -= buffers[idx].len() - off;
            idx += 1;
            off = 0;
        }
        off += n;
    }
    Ok(total)
}

#[test]
fn read_a_file_through_file_reader() {
    let test_io = TinyIo::INIT.io();
    let tmp_dir = TmpDir::new();

    let txt = b"hello minimal test_io\n";
    let contents: Vec<u8> = txt.repeat(100);
    tmp_dir.write_file("test.txt", &contents);

    // Open through our Io. The Dir handle is a plain fd, so it is usable
    // across Io implementations.
    let dir = tmp_dir.dir();
    let file = dir
        .open_file(test_io, b"test.txt", OpenFileOptions::default())
        .unwrap();

    // Stat through our Io.
    let stat = file.stat(test_io).unwrap();
    assert_eq!(contents.len() as u64, stat.size);
    assert_eq!(FileKind::File, stat.kind);

    // fileLength.
    assert_eq!(contents.len() as u64, file.length(test_io).unwrap());

    // Read it all back through the reader (exercises positional reads
    // and streaming fallbacks).
    let mut reader = std::io::BufReader::with_capacity(64, file.reader(test_io));
    let mut list = Vec::new();
    std::io::Read::read_to_end(&mut reader, &mut list).unwrap();
    assert_eq!(contents, list);
    file.close(test_io);
}

#[test]
fn seek() {
    let test_io = TinyIo::INIT.io();
    let tmp_dir = TmpDir::new();
    tmp_dir.write_file("seek.txt", b"0123456789");

    let dir = tmp_dir.dir();
    let file = dir
        .open_file(test_io, b"seek.txt", OpenFileOptions::default())
        .unwrap();

    // Exercise our seek and streaming read implementations directly at
    // the vtable level.
    let mut out = [0u8; 4];

    test_io.file_seek_to(file, 6).unwrap();
    let n = file.read_streaming(test_io, &mut [&mut out[..]]).unwrap();
    assert_eq!(b"6789", &out[..n]);

    // Seek back relative and re-read.
    test_io.file_seek_by(file, -8).unwrap();
    let n = file.read_streaming(test_io, &mut [&mut out[..]]).unwrap();
    assert_eq!(b"2345", &out[..n]);

    // Positional reads are independent of the seek position.
    let n = test_io
        .file_read_positional(file, &mut [&mut out[..]], 1)
        .unwrap();
    assert_eq!(b"1234", &out[..n]);
    file.close(test_io);
}

#[test]
fn real_path_and_delete_file() {
    let test_io = TinyIo::INIT.io();
    let tmp_dir = TmpDir::new();
    tmp_dir.write_file("real.txt", b"x");

    let dir = tmp_dir.dir();
    let file = dir
        .open_file(test_io, b"real.txt", OpenFileOptions::default())
        .unwrap();
    let mut path_buf = vec![0u8; libc::PATH_MAX as usize];
    let len = file.real_path(test_io, &mut path_buf).unwrap();
    let path = path_buf[..len].to_vec();
    assert!(path.ends_with(b"real.txt"));
    file.close(test_io);

    // dirRealPathFile via absolute path from cwd.
    let mut path_buf2 = vec![0u8; libc::PATH_MAX as usize];
    let len2 = Dir::cwd()
        .real_path_file(test_io, &path, &mut path_buf2)
        .unwrap();
    assert_eq!(path, &path_buf2[..len2]);

    // Delete it through our Io, verify it is gone.
    dir.delete_file(test_io, b"real.txt").unwrap();
    assert_eq!(
        Err(Error::FileNotFound),
        dir.open_file(test_io, b"real.txt", OpenFileOptions::default())
    );
}

#[test]
fn open_file_of_a_directory_returns_is_dir() {
    let test_io = TinyIo::INIT.io();
    let tmp_dir = TmpDir::new();
    std::fs::create_dir(tmp_dir.path.join("sub")).unwrap();
    let dir = tmp_dir.dir();
    assert_eq!(
        Err(Error::IsDir),
        dir.open_file(
            test_io,
            b"sub",
            OpenFileOptions {
                allow_directory: false,
                ..Default::default()
            }
        )
    );
}

#[test]
fn io_mutex_through_tinyio() {
    let test_io = TinyIo::INIT.io();
    // Contended and uncontended lock/unlock; exercises the futex ops.
    let mutex = Mutex::INIT;
    mutex.lock_uncancelable(test_io);
    mutex.unlock(test_io);

    // Wake with no waiters must be a no-op.
    let word = AtomicU32::new(0);
    test_io.futex_wake(&word, 1);

    // Wait with a non-matching expected value must return immediately.
    test_io.futex_wait_uncancelable(&word, 1);
}

#[test]
fn random_secure_fills_with_fresh_entropy() {
    let test_io = TinyIo::INIT.io();

    let mut a = [0u8; 32];
    let mut b = [0u8; 32];
    test_io.random_secure(&mut a).unwrap();
    test_io.random_secure(&mut b).unwrap();

    // Non-zero and non-repeating. A zero fill is what `random` does
    // without a source, which would make every one-time password the
    // same; identical draws would mean the same thing.
    assert!(!a.iter().all(|&x| x == 0));
    assert!(!b.iter().all(|&x| x == 0));
    assert_ne!(a, b);

    // Zero-length is a no-op.
    test_io.random_secure(&mut a[..0]).unwrap();
}

#[test]
fn unused_operations_fail_gracefully() {
    let test_io = TinyIo::INIT.io();
    let tmp_dir = TmpDir::new();
    let dir = tmp_dir.dir();
    assert_eq!(
        Err(Error::NoSpaceLeft),
        dir.create_file(test_io, b"nope.txt")
    );
}

#[test]
fn open_file_edge_cases() {
    let test_io = TinyIo::INIT.io();
    let tmp_dir = TmpDir::new();
    tmp_dir.write_file("edge.txt", b"edge");
    let dir = tmp_dir.dir();

    // File locking is unimplemented and must be reported, not ignored.
    assert_eq!(
        Err(Error::FileLocksUnsupported),
        dir.open_file(
            test_io,
            b"edge.txt",
            OpenFileOptions {
                lock: Lock::Shared,
                ..Default::default()
            }
        )
    );

    // NUL bytes never form a valid path.
    assert_eq!(
        Err(Error::BadPathName),
        dir.open_file(test_io, b"bad\x00path", OpenFileOptions::default())
    );

    // Paths that can't fit in PATH_MAX must not be silently truncated.
    let long_name = vec![b'a'; libc::PATH_MAX as usize + 1];
    assert_eq!(
        Err(Error::NameTooLong),
        dir.open_file(test_io, &long_name, OpenFileOptions::default())
    );

    // A path component that is a file, not a directory.
    assert_eq!(
        Err(Error::NotDir),
        dir.open_file(test_io, b"edge.txt/child", OpenFileOptions::default())
    );

    // Directories may be opened when allowed (the default) and stat
    // reports their kind.
    std::fs::create_dir(tmp_dir.path.join("subdir")).unwrap();
    let dir_file = dir
        .open_file(test_io, b"subdir", OpenFileOptions::default())
        .unwrap();
    let dir_stat = dir_file.stat(test_io).unwrap();
    assert_eq!(FileKind::Directory, dir_stat.kind);
    dir_file.close(test_io);

    // Write modes translate to the right ACCMODE flags; TinyIo can't
    // write but opening for write must succeed.
    let wfile = dir
        .open_file(
            test_io,
            b"edge.txt",
            OpenFileOptions {
                mode: OpenMode::WriteOnly,
                ..Default::default()
            },
        )
        .unwrap();
    wfile.close(test_io);
    let rwfile = dir
        .open_file(
            test_io,
            b"edge.txt",
            OpenFileOptions {
                mode: OpenMode::ReadWrite,
                ..Default::default()
            },
        )
        .unwrap();
    rwfile.close(test_io);
}

#[test]
fn open_file_symlink_handling() {
    let test_io = TinyIo::INIT.io();
    let tmp_dir = TmpDir::new();
    tmp_dir.write_file("target.txt", b"target");
    std::os::unix::fs::symlink("target.txt", tmp_dir.path.join("link.txt")).unwrap();
    let dir = tmp_dir.dir();

    // Following symlinks (the default) opens the target...
    let file = dir
        .open_file(test_io, b"link.txt", OpenFileOptions::default())
        .unwrap();
    assert_eq!(b"target".len() as u64, file.length(test_io).unwrap());

    // ...and realPath resolves through the link to the target. This is
    // the property the Kitty graphics path validation relies on.
    let mut path_buf = vec![0u8; libc::PATH_MAX as usize];
    let len = file.real_path(test_io, &mut path_buf).unwrap();
    assert!(path_buf[..len].ends_with(b"target.txt"));
    file.close(test_io);

    // Refusing to follow symlinks fails with SymLinkLoop.
    assert_eq!(
        Err(Error::SymLinkLoop),
        dir.open_file(
            test_io,
            b"link.txt",
            OpenFileOptions {
                follow_symlinks: false,
                ..Default::default()
            }
        )
    );
}

#[test]
fn positional_reads_at_and_beyond_eof_return_zero() {
    let test_io = TinyIo::INIT.io();
    let tmp_dir = TmpDir::new();
    tmp_dir.write_file("eof.txt", b"0123456789");
    let dir = tmp_dir.dir();
    let file = dir
        .open_file(test_io, b"eof.txt", OpenFileOptions::default())
        .unwrap();

    let mut out = [0u8; 4];

    // At EOF and past EOF: the vtable contract is "returns 0 if reading
    // at or past the end".
    assert_eq!(
        Ok(0),
        test_io.file_read_positional(file, &mut [&mut out[..]], 10)
    );
    assert_eq!(
        Ok(0),
        test_io.file_read_positional(file, &mut [&mut out[..]], 9999)
    );

    // No buffers and only-empty buffers read nothing without a syscall.
    assert_eq!(Ok(0), test_io.file_read_positional(file, &mut [], 0));
    let mut e1: [u8; 0] = [];
    let mut e2: [u8; 0] = [];
    assert_eq!(
        Ok(0),
        test_io.file_read_positional(file, &mut [&mut e1[..], &mut e2[..]], 0)
    );
    file.close(test_io);
}

#[test]
fn vectored_reads_scatter_across_buffers() {
    let test_io = TinyIo::INIT.io();
    let tmp_dir = TmpDir::new();
    tmp_dir.write_file("vec.txt", b"0123456789abcdef");
    let dir = tmp_dir.dir();
    let file = dir
        .open_file(test_io, b"vec.txt", OpenFileOptions::default())
        .unwrap();

    // Scatter a positional read across multiple buffers, with empty
    // buffers interleaved (they must be skipped).
    let mut a = [0u8; 4];
    let mut b = [0u8; 2];
    let mut c = [0u8; 6];
    let mut empty: [u8; 0] = [];
    let n = test_io
        .file_read_positional(
            file,
            &mut [&mut a[..], &mut empty[..], &mut b[..], &mut c[..]],
            0,
        )
        .unwrap();
    assert_eq!(12, n);
    assert_eq!(b"0123", &a);
    assert_eq!(b"45", &b);
    assert_eq!(b"6789ab", &c);

    // More buffers than MAX_IOVECS_LEN: reads are truncated to the
    // first MAX_IOVECS_LEN non-empty buffers (partial reads are allowed
    // by the vtable contract; callers retry).
    const _: () = assert!(MAX_IOVECS_LEN < 16);
    let mut bytes = [[0u8; 1]; 16];
    let mut many: Vec<&mut [u8]> = bytes.iter_mut().map(|b| &mut b[..]).collect();
    let n2 = test_io.file_read_positional(file, &mut many, 0).unwrap();
    assert_eq!(MAX_IOVECS_LEN, n2);
    for (got, want) in bytes[..n2].iter().zip(&b"0123456789abcdef"[..n2]) {
        assert_eq!(*want, got[0]);
    }
    file.close(test_io);
}

#[test]
fn streaming_reads_end_of_stream_and_scatter() {
    let test_io = TinyIo::INIT.io();
    let tmp_dir = TmpDir::new();
    tmp_dir.write_file("stream.txt", b"streaming!");
    let dir = tmp_dir.dir();
    let file = dir
        .open_file(test_io, b"stream.txt", OpenFileOptions::default())
        .unwrap();

    // Scatter a streaming read (one readv on POSIX).
    let mut a = [0u8; 6];
    let mut b = [0u8; 4];
    assert_eq!(
        Ok(10),
        read_streaming_all(file, test_io, &mut [&mut a[..], &mut b[..]])
    );
    assert_eq!(b"stream", &a);
    assert_eq!(b"ing!", &b);

    // Reading again at EOF is a stream end, not a zero-length success.
    assert_eq!(
        Err(Error::EndOfStream),
        file.read_streaming(test_io, &mut [&mut a[..], &mut b[..]])
    );

    // Empty destinations read nothing.
    assert_eq!(Ok(0), file.read_streaming(test_io, &mut []));

    // Seeking beyond EOF is legal; the next streaming read hits EOF.
    test_io.file_seek_to(file, 9999).unwrap();
    assert_eq!(
        Err(Error::EndOfStream),
        file.read_streaming(test_io, &mut [&mut a[..], &mut b[..]])
    );

    // And seeking back to zero re-reads from the start.
    test_io.file_seek_to(file, 0).unwrap();
    assert_eq!(
        Ok(10),
        read_streaming_all(file, test_io, &mut [&mut a[..], &mut b[..]])
    );
    assert_eq!(b"stream", &a);
    file.close(test_io);
}

#[test]
fn pipes_streaming_works_positional_and_seek_are_unseekable() {
    let test_io = TinyIo::INIT.io();

    let msg = b"through the pipe";
    let mut fds = [0i32; 2];
    // SAFETY: valid out array.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return;
    }
    // SAFETY: valid fd and buffer.
    let written = unsafe { libc::write(fds[1], msg.as_ptr().cast(), msg.len()) };
    assert_eq!(msg.len() as isize, written);
    // SAFETY: closing our write end.
    unsafe { libc::close(fds[1]) };
    let read_end = File {
        handle: fds[0],
        flags: FileFlags { nonblocking: false },
    };

    // Streaming reads work on unseekable files; this is the fallback
    // File.Reader depends on when positional reads report Unseekable.
    let mut buf = [0u8; 16];
    assert_eq!(
        Ok(msg.len()),
        read_end.read_streaming(test_io, &mut [&mut buf[..]])
    );
    assert_eq!(msg, &buf);

    // Positional reads and seeks must report Unseekable so callers can
    // fall back to streaming.
    assert_eq!(
        Err(Error::Unseekable),
        test_io.file_read_positional(read_end, &mut [&mut buf[..]], 0)
    );
    assert_eq!(Err(Error::Unseekable), test_io.file_seek_to(read_end, 0));
    assert_eq!(Err(Error::Unseekable), test_io.file_seek_by(read_end, 1));
    read_end.close(test_io);
}

#[test]
fn operate_delegates_non_read_operations_to_failing_stubs() {
    let test_io = TinyIo::INIT.io();
    let tmp_dir = TmpDir::new();
    tmp_dir.write_file("w.txt", b"x");
    let dir = tmp_dir.dir();
    let file = dir
        .open_file(
            test_io,
            b"w.txt",
            OpenFileOptions {
                mode: OpenMode::WriteOnly,
                ..Default::default()
            },
        )
        .unwrap();

    let result = test_io.operate(Operation::FileWriteStreaming {
        file,
        data: &[&b"nope"[..]],
    });
    assert_eq!(
        OperationResult::FileWriteStreaming(Err(Error::InputOutput)),
        result
    );
    file.close(test_io);
}

#[test]
fn dir_real_path_file_edge_cases() {
    let test_io = TinyIo::INIT.io();
    let tmp_dir = TmpDir::new();
    tmp_dir.write_file("real.txt", b"x");
    let dir = tmp_dir.dir();

    // Resolve the canonical path through an open file for reference.
    let file = dir
        .open_file(test_io, b"real.txt", OpenFileOptions::default())
        .unwrap();
    let mut want_buf = vec![0u8; libc::PATH_MAX as usize];
    let want_len = file.real_path(test_io, &mut want_buf).unwrap();
    let want = want_buf[..want_len].to_vec();
    file.close(test_io);

    // A non-cwd directory handle exercises the open-then-resolve
    // fallback branch rather than libc realpath.
    let mut got_buf = vec![0u8; libc::PATH_MAX as usize];
    let got_len = test_io
        .dir_real_path_file(dir, b"real.txt", &mut got_buf)
        .unwrap();
    assert_eq!(want, &got_buf[..got_len]);

    // Missing paths report FileNotFound (libc realpath branch, via an
    // absolute path anchored at cwd).
    let mut missing = want.clone();
    missing.extend_from_slice(b".missing");
    let mut out_buf = vec![0u8; libc::PATH_MAX as usize];
    assert_eq!(
        Err(Error::FileNotFound),
        Dir::cwd().real_path_file(test_io, &missing, &mut out_buf)
    );

    // libc realpath requires a PATH_MAX-sized output buffer; smaller
    // buffers must error rather than risk truncation.
    let mut small_buf = [0u8; 8];
    assert_eq!(
        Err(Error::NameTooLong),
        Dir::cwd().real_path_file(test_io, &want, &mut small_buf)
    );
}

#[test]
fn delete_file_edge_cases() {
    let test_io = TinyIo::INIT.io();
    let tmp_dir = TmpDir::new();
    let dir = tmp_dir.dir();

    // Nonexistent files.
    assert_eq!(
        Err(Error::FileNotFound),
        dir.delete_file(test_io, b"missing.txt")
    );

    // NUL bytes never form a valid POSIX path.
    assert_eq!(
        Err(Error::BadPathName),
        dir.delete_file(test_io, b"bad\x00path")
    );

    // Deleting a directory reports IsDir. On BSD-derived systems
    // (including macOS) unlink returns EPERM for directories, which
    // exercises the stat-based disambiguation path.
    std::fs::create_dir(tmp_dir.path.join("subdir")).unwrap();
    assert_eq!(Err(Error::IsDir), dir.delete_file(test_io, b"subdir"));

    // Deleting a symlink removes the link, not its target.
    tmp_dir.write_file("target.txt", b"x");
    std::os::unix::fs::symlink("target.txt", tmp_dir.path.join("link.txt")).unwrap();
    dir.delete_file(test_io, b"link.txt").unwrap();
    let file = dir
        .open_file(test_io, b"target.txt", OpenFileOptions::default())
        .unwrap();
    file.close(test_io);
    assert_eq!(
        Err(Error::FileNotFound),
        dir.open_file(test_io, b"link.txt", OpenFileOptions::default())
    );
}

#[test]
fn dir_close_closes_the_descriptor() {
    let test_io = TinyIo::INIT.io();
    let tmp_dir = TmpDir::new();
    std::fs::create_dir(tmp_dir.path.join("subdir")).unwrap();
    let opened = std::fs::File::open(tmp_dir.path.join("subdir")).unwrap();
    let dir = Dir {
        handle: std::os::fd::IntoRawFd::into_raw_fd(opened),
    };
    test_io.dir_close(&[dir]);

    // Verify with a raw dup that the fd is gone. We check the errno
    // directly rather than going through our Io.
    // SAFETY: dup on a (closed) fd number is harmless.
    let rc = unsafe { libc::dup(dir.handle) };
    assert_eq!(-1, rc);
    assert_eq!(
        Some(libc::EBADF),
        std::io::Error::last_os_error().raw_os_error()
    );
}

#[test]
fn cancel_protection_operations_are_benign() {
    let test_io = TinyIo::INIT.io();

    // There is no async, so cancelation can never be requested.
    test_io.check_cancel().unwrap();

    // Swap and restore roundtrip like generic code does around
    // uninterruptible sections.
    let prev = test_io.swap_cancel_protection(CancelProtection::Blocked);
    let _ = test_io.swap_cancel_protection(prev);
    test_io.recancel();
}

#[test]
fn async_runs_inline_concurrency_is_unavailable() {
    let test_io = TinyIo::INIT.io();

    fn work(x: &mut u32) -> u32 {
        *x += 1;
        *x
    }

    // The task must have executed synchronously, before await.
    let mut state: u32 = 41;
    let future = test_io.async_(|| work(&mut state));
    assert_eq!(42, state);
    assert_eq!(42, future.await_(test_io));

    // Concurrency must be reported as unavailable, not silently run.
    assert!(matches!(
        test_io.concurrent(|| work(&mut state)),
        Err(Error::ConcurrencyUnavailable)
    ));
    assert_eq!(42, state);
}

#[test]
fn futex_timed_waits_return() {
    let test_io = TinyIo::INIT.io();

    // Nobody wakes this futex, so returning at all proves the timeout
    // (or the spurious-wakeup contract) works.
    let word = AtomicU32::new(1);
    test_io
        .futex_wait(&word, 1, Timeout::Duration(Duration::from_millis(5)))
        .unwrap();

    // Deadlines can't be resolved without a clock; they degrade to a
    // short poll which must also return.
    test_io
        .futex_wait(
            &word,
            1,
            Timeout::Deadline(Instant::now() + Duration::from_nanos(1)),
        )
        .unwrap();

    // A mismatched expected value returns immediately even with no
    // timeout.
    test_io.futex_wait(&word, 2, Timeout::None).unwrap();

    // Waking more than one waiter takes the wake-all path.
    test_io.futex_wake(&word, 2);
    test_io.futex_wake(&word, u32::MAX);
}

#[test]
fn io_mutex_under_real_thread_contention() {
    let test_io = TinyIo::INIT.io();

    // Hammer a mutex from several threads so waiters actually park in
    // futexWait and get released by futexWake.
    const ITERATIONS: u64 = 10_000;
    const THREAD_COUNT: u64 = 4;

    let mutex = Mutex::INIT;
    // A non-atomic read-modify-write guarded only by the mutex.
    let counter = AtomicU64::new(0);

    std::thread::scope(|s| {
        for _ in 0..THREAD_COUNT {
            s.spawn(|| {
                for _ in 0..ITERATIONS {
                    mutex.lock_uncancelable(test_io);
                    let v = counter.load(Ordering::Relaxed);
                    counter.store(v + 1, Ordering::Relaxed);
                    mutex.unlock(test_io);
                }
            });
        }
    });

    assert_eq!(ITERATIONS * THREAD_COUNT, counter.load(Ordering::Relaxed));
}
