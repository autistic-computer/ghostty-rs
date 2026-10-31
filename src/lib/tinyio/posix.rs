//! Port of `src/lib/tinyio/posix.zig`.
//!
//! STATUS: ported
//!
//! POSIX impl of TinyIo: plain blocking syscalls cribbed from
//! std.posix and the POSIX impls of `std.Io.Threaded`.

#![cfg(unix)]

use std::ffi::{c_int, CString};
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicU32, Ordering};

use super::{
    Dir, Error, File, FileFlags, FileKind, Lock, OpenFileOptions, OpenMode, Stat, MAX_IOVECS_LEN,
};

const PATH_MAX: usize = libc::PATH_MAX as usize;

fn errno() -> c_int {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

fn unexpected_errno(err: c_int) -> Error {
    if cfg!(debug_assertions) {
        log::warn!(target: "tinyio", "unexpected errno: {err}");
    }
    Error::Unexpected
}

/// Zig: `std.Io.Threaded.pathToPosix`. Paths containing NUL are never
/// valid and paths that can't fit in PATH_MAX must not be truncated.
fn path_to_posix(path: &[u8]) -> Result<CString, Error> {
    if path.contains(&0) {
        return Err(Error::BadPathName);
    }
    if path.len() >= PATH_MAX {
        return Err(Error::NameTooLong);
    }
    CString::new(path).map_err(|_| Error::BadPathName)
}

pub fn random_secure(buffer: &mut [u8]) -> Result<(), Error> {
    // The same sources as `std.Io.Threaded.randomSecure` minus
    // cancelation and the /dev/urandom fallback: arc4random_buf where
    // libc provides it (all the BSDs and Darwin), otherwise the getrandom
    // syscall on Linux. Anything else has no entropy.
    #[cfg(any(
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "dragonfly"
    ))]
    {
        // SAFETY: buffer is valid for writes of its length.
        unsafe { libc::arc4random_buf(buffer.as_mut_ptr().cast(), buffer.len()) };
        return Ok(());
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let mut i = 0usize;
        while i < buffer.len() {
            // SAFETY: the remaining buffer is valid for writes.
            let rc =
                unsafe { libc::getrandom(buffer[i..].as_mut_ptr().cast(), buffer.len() - i, 0) };
            if rc >= 0 {
                i += rc as usize;
                continue;
            }
            match errno() {
                libc::EINTR => continue,
                _ => return Err(Error::EntropyUnavailable),
            }
        }
        return Ok(());
    }

    #[allow(unreachable_code)]
    Err(Error::EntropyUnavailable)
}

pub fn close_fd(fd: RawFd) {
    // Never retry close on EINTR: POSIX leaves the fd state unspecified
    // and Linux always closes it, so retrying risks closing an unrelated
    // fd opened by another thread.
    // SAFETY: closing an fd has no memory-safety preconditions.
    unsafe { libc::close(fd) };
}

pub fn dir_open_file(dir: Dir, sub_path: &[u8], options: OpenFileOptions) -> Result<File, Error> {
    let sub_path_posix = path_to_posix(sub_path)?;

    // Nothing in the terminal locks files. Implementing this requires
    // flock fallbacks (see std.Io.Threaded); report it as unsupported.
    if options.lock != Lock::None {
        return Err(Error::FileLocksUnsupported);
    }

    let mut flags: c_int = match options.mode {
        OpenMode::ReadOnly => libc::O_RDONLY,
        OpenMode::WriteOnly => libc::O_WRONLY,
        OpenMode::ReadWrite => libc::O_RDWR,
    };
    if !options.follow_symlinks {
        flags |= libc::O_NOFOLLOW;
    }
    flags |= libc::O_CLOEXEC;
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        flags |= libc::O_LARGEFILE;
    }
    if !options.allow_ctty {
        flags |= libc::O_NOCTTY;
    }
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "freebsd"))]
    if options.path_only {
        flags |= libc::O_PATH;
    }
    #[cfg(target_os = "freebsd")]
    if options.resolve_beneath {
        flags |= libc::O_RESOLVE_BENEATH;
    }

    let mode: libc::mode_t = 0;
    let fd: RawFd = loop {
        // SAFETY: path is NUL-terminated.
        let rc = unsafe {
            libc::openat(
                dir.handle,
                sub_path_posix.as_ptr(),
                flags,
                mode as libc::c_uint,
            )
        };
        if rc >= 0 {
            break rc;
        }
        return Err(match errno() {
            libc::EINTR => continue,
            libc::EINVAL => Error::BadPathName,
            libc::EACCES => Error::AccessDenied,
            libc::EFBIG => Error::FileTooBig,
            libc::EOVERFLOW => Error::FileTooBig,
            libc::EISDIR => Error::IsDir,
            libc::ELOOP => Error::SymLinkLoop,
            // FreeBSD reports O_NOFOLLOW on a symlink as EMLINK, not ELOOP.
            libc::EMLINK => Error::SymLinkLoop,
            libc::EMFILE => Error::ProcessFdQuotaExceeded,
            libc::ENAMETOOLONG => Error::NameTooLong,
            libc::ENFILE => Error::SystemFdQuotaExceeded,
            libc::ENODEV => Error::NoDevice,
            libc::ENOENT => Error::FileNotFound,
            libc::ESRCH => Error::FileNotFound,
            libc::ENOMEM => Error::SystemResources,
            libc::ENOSPC => Error::NoSpaceLeft,
            libc::ENOTDIR => Error::NotDir,
            libc::EPERM => Error::PermissionDenied,
            libc::EEXIST => Error::PathAlreadyExists,
            libc::EBUSY => Error::DeviceBusy,
            libc::EOPNOTSUPP => Error::FileLocksUnsupported,
            libc::EAGAIN => Error::WouldBlock,
            libc::ETXTBSY => Error::FileBusy,
            libc::ENXIO => Error::NoDevice,
            libc::EROFS => Error::ReadOnlyFileSystem,
            libc::EILSEQ => Error::BadPathName,
            err => unexpected_errno(err),
        });
    };

    let file = File {
        handle: fd,
        flags: FileFlags { nonblocking: false },
    };

    if !options.allow_directory {
        let is_dir = match file_stat(file) {
            Ok(stat) => stat.kind == FileKind::Directory,
            // Directory-ness is unknown or unknowable.
            Err(Error::Streaming) => false,
            Err(e) => {
                close_fd(fd);
                return Err(e);
            }
        };
        if is_dir {
            close_fd(fd);
            return Err(Error::IsDir);
        }
    }

    Ok(file)
}

pub fn dir_close(dirs: &[Dir]) {
    for dir in dirs {
        close_fd(dir.handle);
    }
}

pub fn file_close(files: &[File]) {
    for file in files {
        close_fd(file.handle);
    }
}

fn kind_from_mode(mode: u32) -> FileKind {
    match mode & libc::S_IFMT as u32 {
        m if m == libc::S_IFBLK as u32 => FileKind::BlockDevice,
        m if m == libc::S_IFCHR as u32 => FileKind::CharacterDevice,
        m if m == libc::S_IFDIR as u32 => FileKind::Directory,
        m if m == libc::S_IFIFO as u32 => FileKind::NamedPipe,
        m if m == libc::S_IFLNK as u32 => FileKind::SymLink,
        m if m == libc::S_IFREG as u32 => FileKind::File,
        m if m == libc::S_IFSOCK as u32 => FileKind::UnixDomainSocket,
        _ => FileKind::Unknown,
    }
}

pub fn file_stat(file: File) -> Result<Stat, Error> {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        // Zig: `std.Io.Threaded.linux_statx_request`.
        let request = libc::STATX_TYPE
            | libc::STATX_MODE
            | libc::STATX_ATIME
            | libc::STATX_MTIME
            | libc::STATX_CTIME
            | libc::STATX_INO
            | libc::STATX_SIZE
            | libc::STATX_NLINK;
        loop {
            // SAFETY: statx is plain old data.
            let mut statx: libc::statx = unsafe { std::mem::zeroed() };
            // SAFETY: valid fd, empty NUL-terminated path, valid out pointer.
            let rc = unsafe {
                libc::statx(
                    file.handle,
                    c"".as_ptr(),
                    libc::AT_EMPTY_PATH,
                    request,
                    &mut statx,
                )
            };
            if rc == 0 {
                // Zig: `std.Io.Threaded.statFromLinux`.
                let ts =
                    |t: libc::statx_timestamp| t.tv_sec as i128 * 1_000_000_000 + t.tv_nsec as i128;
                return Ok(Stat {
                    inode: statx.stx_ino,
                    size: statx.stx_size,
                    permissions: statx.stx_mode as u32 & 0o7777,
                    kind: kind_from_mode(statx.stx_mode as u32),
                    atime: ts(statx.stx_atime),
                    mtime: ts(statx.stx_mtime),
                    ctime: ts(statx.stx_ctime),
                    nlink: statx.stx_nlink as u64,
                    block_size: statx.stx_blksize,
                });
            }
            match errno() {
                libc::EINTR => continue,
                libc::ENOMEM => return Err(Error::SystemResources),
                err => return Err(unexpected_errno(err)),
            }
        }
    }

    #[allow(unreachable_code)]
    loop {
        // SAFETY: stat is plain old data.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: valid fd and out pointer.
        if unsafe { libc::fstat(file.handle, &mut stat) } == 0 {
            // Zig: `std.Io.Threaded.statFromPosix`.
            return Ok(Stat {
                inode: stat.st_ino as u64,
                size: stat.st_size as u64,
                permissions: stat.st_mode as u32 & 0o7777,
                kind: kind_from_mode(stat.st_mode as u32),
                atime: stat.st_atime as i128 * 1_000_000_000 + stat.st_atime_nsec as i128,
                mtime: stat.st_mtime as i128 * 1_000_000_000 + stat.st_mtime_nsec as i128,
                ctime: stat.st_ctime as i128 * 1_000_000_000 + stat.st_ctime_nsec as i128,
                nlink: stat.st_nlink as u64,
                block_size: stat.st_blksize as u32,
            });
        }
        match errno() {
            libc::EINTR => continue,
            libc::ENOMEM => return Err(Error::SystemResources),
            libc::EACCES => return Err(Error::AccessDenied),
            err => return Err(unexpected_errno(err)),
        }
    }
}

pub fn file_length(file: File) -> Result<u64, Error> {
    let stat = file_stat(file)?;
    Ok(stat.size)
}

/// Gathers non-empty buffers into iovecs. Returns the number of iovecs
/// filled; zero means there is nothing to read into.
fn buffers_to_iovecs(
    data: &mut [&mut [u8]],
    iovecs_buffer: &mut [libc::iovec; MAX_IOVECS_LEN],
) -> usize {
    let mut i = 0;
    for buf in data.iter_mut() {
        if iovecs_buffer.len() - i == 0 {
            break;
        }
        if !buf.is_empty() {
            iovecs_buffer[i] = libc::iovec {
                iov_base: buf.as_mut_ptr().cast(),
                iov_len: buf.len(),
            };
            i += 1;
        }
    }
    i
}

const EMPTY_IOVEC: libc::iovec = libc::iovec {
    iov_base: std::ptr::null_mut(),
    iov_len: 0,
};

pub fn file_read_positional(
    file: File,
    data: &mut [&mut [u8]],
    offset: u64,
) -> Result<usize, Error> {
    let mut iovecs_buffer = [EMPTY_IOVEC; MAX_IOVECS_LEN];
    let len = buffers_to_iovecs(data, &mut iovecs_buffer);
    if len == 0 {
        return Ok(0);
    }

    loop {
        // SAFETY: iovecs point into live, exclusively borrowed buffers.
        let rc = unsafe {
            libc::preadv(
                file.handle,
                iovecs_buffer.as_ptr(),
                len as c_int,
                offset as libc::off_t,
            )
        };
        if rc >= 0 {
            return Ok(rc as usize);
        }
        return Err(match errno() {
            libc::EINTR | libc::ETIMEDOUT => continue,
            libc::ENXIO => Error::Unseekable,
            libc::ESPIPE => Error::Unseekable,
            libc::EOVERFLOW => Error::Unseekable,
            libc::ENOBUFS => Error::SystemResources,
            libc::ENOMEM => Error::SystemResources,
            libc::EAGAIN => Error::WouldBlock,
            libc::EIO => Error::InputOutput,
            libc::EISDIR => Error::IsDir,
            libc::EBADF => Error::NotOpenForReading,
            err => unexpected_errno(err),
        });
    }
}

pub fn file_read_streaming(file: File, data: &mut [&mut [u8]]) -> Result<usize, Error> {
    let mut iovecs_buffer = [EMPTY_IOVEC; MAX_IOVECS_LEN];
    let len = buffers_to_iovecs(data, &mut iovecs_buffer);
    if len == 0 {
        return Ok(0);
    }

    loop {
        // SAFETY: iovecs point into live, exclusively borrowed buffers.
        let rc = unsafe { libc::readv(file.handle, iovecs_buffer.as_ptr(), len as c_int) };
        if rc > 0 {
            return Ok(rc as usize);
        }
        if rc == 0 {
            return Err(Error::EndOfStream);
        }
        return Err(match errno() {
            libc::EINTR | libc::ETIMEDOUT => continue,
            libc::EAGAIN => Error::WouldBlock,
            libc::EIO => Error::InputOutput,
            libc::EISDIR => Error::IsDir,
            libc::ENOBUFS => Error::SystemResources,
            libc::ENOMEM => Error::SystemResources,
            libc::ENOTCONN => Error::SocketUnconnected,
            libc::ECONNRESET => Error::ConnectionResetByPeer,
            libc::EBADF => Error::NotOpenForReading,
            err => unexpected_errno(err),
        });
    }
}

fn lseek(file: File, offset: i64, whence: c_int) -> Result<(), Error> {
    loop {
        // SAFETY: lseek has no memory-safety preconditions.
        let rc = unsafe { libc::lseek(file.handle, offset as libc::off_t, whence) };
        if rc >= 0 {
            return Ok(());
        }
        return Err(match errno() {
            libc::EINTR => continue,
            libc::EINVAL => Error::Unseekable,
            libc::EOVERFLOW => Error::Unseekable,
            libc::ESPIPE => Error::Unseekable,
            libc::ENXIO => Error::Unseekable,
            err => unexpected_errno(err),
        });
    }
}

pub fn file_seek_by(file: File, offset: i64) -> Result<(), Error> {
    lseek(file, offset, libc::SEEK_CUR)
}

pub fn file_seek_to(file: File, offset: u64) -> Result<(), Error> {
    lseek(file, offset as i64, libc::SEEK_SET)
}

pub fn file_real_path(file: File, out_buffer: &mut [u8]) -> Result<usize, Error> {
    real_path_fd(file.handle, out_buffer)
}

fn real_path_fd(fd: RawFd, out_buffer: &mut [u8]) -> Result<usize, Error> {
    #[cfg(any(target_vendor = "apple", target_os = "dragonfly"))]
    {
        let mut sufficient_buffer = [0u8; PATH_MAX];
        loop {
            // SAFETY: F_GETPATH writes at most PATH_MAX bytes.
            if unsafe { libc::fcntl(fd, libc::F_GETPATH, sufficient_buffer.as_mut_ptr()) } != -1 {
                break;
            }
            return Err(match errno() {
                libc::EINTR => continue,
                libc::EACCES => Error::AccessDenied,
                libc::EBADF => Error::FileNotFound,
                libc::ENOENT => Error::FileNotFound,
                libc::ENOMEM => Error::SystemResources,
                libc::ENOSPC => Error::NameTooLong,
                libc::ERANGE => Error::NameTooLong,
                err => unexpected_errno(err),
            });
        }
        let n = sufficient_buffer
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(sufficient_buffer.len());
        if n > out_buffer.len() {
            return Err(Error::NameTooLong);
        }
        out_buffer[..n].copy_from_slice(&sufficient_buffer[..n]);
        return Ok(n);
    }

    #[cfg(any(target_os = "linux", target_os = "android", target_os = "illumos"))]
    {
        let template = if cfg!(target_os = "illumos") {
            "/proc/self/path/"
        } else {
            "/proc/self/fd/"
        };
        let proc_path = CString::new(format!("{template}{fd}")).unwrap();
        loop {
            // SAFETY: valid NUL-terminated path and output buffer.
            let rc = unsafe {
                libc::readlink(
                    proc_path.as_ptr(),
                    out_buffer.as_mut_ptr().cast(),
                    out_buffer.len(),
                )
            };
            if rc >= 0 {
                return Ok(rc as usize);
            }
            return Err(match errno() {
                libc::EINTR => continue,
                libc::EACCES => Error::AccessDenied,
                libc::EIO => Error::FileSystem,
                libc::ELOOP => Error::SymLinkLoop,
                libc::ENAMETOOLONG => Error::NameTooLong,
                libc::ENOENT => Error::FileNotFound,
                libc::ENOMEM => Error::SystemResources,
                libc::ENOTDIR => Error::NotDir,
                err => unexpected_errno(err),
            });
        }
    }

    #[cfg(target_os = "freebsd")]
    {
        // SAFETY: kinfo_file is plain old data.
        let mut k_file: libc::kinfo_file = unsafe { std::mem::zeroed() };
        k_file.kf_structsize = libc::KINFO_FILE_SIZE;
        loop {
            // SAFETY: F_KINFO fills the struct.
            if unsafe { libc::fcntl(fd, libc::F_KINFO, &mut k_file as *mut _ as usize) } != -1 {
                break;
            }
            return Err(match errno() {
                libc::EINTR => continue,
                libc::EBADF => Error::FileNotFound,
                err => unexpected_errno(err),
            });
        }
        let path: &[u8] = unsafe {
            std::slice::from_raw_parts(k_file.kf_path.as_ptr().cast(), k_file.kf_path.len())
        };
        let len = path.iter().position(|&b| b == 0).unwrap_or(path.len());
        if len == 0 {
            return Err(Error::NameTooLong);
        }
        out_buffer[..len].copy_from_slice(&path[..len]);
        return Ok(len);
    }

    #[allow(unreachable_code)]
    Err(Error::OperationUnsupported)
}

pub fn dir_real_path_file(
    dir: Dir,
    sub_path: &[u8],
    out_buffer: &mut [u8],
) -> Result<usize, Error> {
    let sub_path_posix = path_to_posix(sub_path)?;

    if dir.handle == libc::AT_FDCWD {
        if out_buffer.len() < PATH_MAX {
            return Err(Error::NameTooLong);
        }
        loop {
            // SAFETY: out_buffer holds at least PATH_MAX bytes.
            let p =
                unsafe { libc::realpath(sub_path_posix.as_ptr(), out_buffer.as_mut_ptr().cast()) };
            if !p.is_null() {
                debug_assert_eq!(p as *const u8, out_buffer.as_ptr());
                return Ok(out_buffer
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(out_buffer.len()));
            }
            return Err(match errno() {
                libc::EINTR => continue,
                libc::EACCES => Error::AccessDenied,
                libc::ENOENT => Error::FileNotFound,
                libc::EOPNOTSUPP => Error::OperationUnsupported,
                libc::ENOTDIR => Error::NotDir,
                libc::ENAMETOOLONG => Error::NameTooLong,
                libc::ELOOP => Error::SymLinkLoop,
                libc::EIO => Error::InputOutput,
                err => unexpected_errno(err),
            });
        }
    }

    // Fallback: open the path and resolve the fd. Used for non-cwd
    // directory handles (which the terminal itself never passes).
    #[allow(unused_mut)]
    let mut flags: c_int = libc::O_NONBLOCK | libc::O_CLOEXEC;
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "freebsd"))]
    {
        flags |= libc::O_PATH;
    }

    let fd: RawFd = loop {
        // SAFETY: path is NUL-terminated.
        let rc = unsafe {
            libc::openat(
                dir.handle,
                sub_path_posix.as_ptr(),
                flags,
                0 as libc::c_uint,
            )
        };
        if rc >= 0 {
            break rc;
        }
        return Err(match errno() {
            libc::EINTR => continue,
            libc::EINVAL => Error::BadPathName,
            libc::EACCES => Error::AccessDenied,
            libc::ELOOP => Error::SymLinkLoop,
            libc::ENAMETOOLONG => Error::NameTooLong,
            libc::ENOENT => Error::FileNotFound,
            libc::ENOMEM => Error::SystemResources,
            libc::ENOTDIR => Error::NotDir,
            libc::EILSEQ => Error::BadPathName,
            err => unexpected_errno(err),
        });
    };

    let result = real_path_fd(fd, out_buffer);
    close_fd(fd);
    result
}

pub fn dir_delete_file(dir: Dir, sub_path: &[u8]) -> Result<(), Error> {
    let sub_path_posix = path_to_posix(sub_path)?;

    loop {
        // SAFETY: path is NUL-terminated.
        if unsafe { libc::unlinkat(dir.handle, sub_path_posix.as_ptr(), 0) } == 0 {
            return Ok(());
        }
        return Err(match errno() {
            libc::EINTR => continue,
            // Some systems return EPERM when trying to delete a directory;
            // stat to disambiguate from a real permission error.
            libc::EPERM => {
                if cfg!(any(
                    target_vendor = "apple",
                    target_os = "freebsd",
                    target_os = "netbsd",
                    target_os = "dragonfly",
                    target_os = "openbsd",
                    target_os = "illumos"
                )) {
                    // SAFETY: stat is plain old data.
                    let mut st: libc::stat = unsafe { std::mem::zeroed() };
                    loop {
                        // SAFETY: valid path and out pointer.
                        if unsafe {
                            libc::fstatat(
                                dir.handle,
                                sub_path_posix.as_ptr(),
                                &mut st,
                                libc::AT_SYMLINK_NOFOLLOW,
                            )
                        } == 0
                        {
                            break;
                        }
                        match errno() {
                            libc::EINTR => continue,
                            _ => return Err(Error::PermissionDenied),
                        }
                    }
                    let is_dir = st.st_mode & libc::S_IFMT == libc::S_IFDIR;
                    if is_dir {
                        Error::IsDir
                    } else {
                        Error::PermissionDenied
                    }
                } else {
                    Error::PermissionDenied
                }
            }
            libc::EACCES => Error::AccessDenied,
            libc::EBUSY => Error::FileBusy,
            libc::EIO => Error::FileSystem,
            libc::EISDIR => Error::IsDir,
            libc::ELOOP => Error::SymLinkLoop,
            libc::ENAMETOOLONG => Error::NameTooLong,
            libc::ENOENT => Error::FileNotFound,
            libc::ENOTDIR => Error::NotDir,
            libc::ENOMEM => Error::SystemResources,
            libc::EROFS => Error::ReadOnlyFileSystem,
            libc::EILSEQ => Error::BadPathName,
            err => unexpected_errno(err),
        });
    }
}

#[cfg(target_vendor = "apple")]
extern "C" {
    fn __ulock_wait(operation: u32, addr: *mut libc::c_void, value: u64, timeout_us: u32) -> c_int;
    fn __ulock_wake(operation: u32, addr: *mut libc::c_void, wake_value: u64) -> c_int;
}
#[cfg(target_vendor = "apple")]
const UL_COMPARE_AND_WAIT: u32 = 1;
#[cfg(target_vendor = "apple")]
const ULF_WAKE_ALL: u32 = 0x0000_0100;
#[cfg(target_vendor = "apple")]
const ULF_NO_ERRNO: u32 = 0x0100_0000;

#[cfg(target_os = "freebsd")]
extern "C" {
    fn _umtx_op(
        obj: *mut libc::c_void,
        op: c_int,
        val: libc::c_ulong,
        uaddr: *mut libc::c_void,
        uaddr2: *mut libc::c_void,
    ) -> c_int;
}
#[cfg(target_os = "freebsd")]
const UMTX_OP_WAIT_UINT_PRIVATE: c_int = 15;
#[cfg(target_os = "freebsd")]
const UMTX_OP_WAKE_PRIVATE: c_int = 16;
#[cfg(target_os = "freebsd")]
#[repr(C)]
struct UmtxTime {
    timeout: libc::timespec,
    flags: u32,
    clockid: u32,
}

#[cold]
pub fn futex_wait_inner(ptr: &AtomicU32, expected: u32, timeout_ns: Option<u64>) {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let ts_buffer;
        let ts: *const libc::timespec = match timeout_ns {
            Some(ns) => {
                ts_buffer = libc::timespec {
                    tv_sec: (ns / 1_000_000_000) as libc::time_t,
                    tv_nsec: (ns % 1_000_000_000) as libc::c_long,
                };
                &ts_buffer
            }
            None => std::ptr::null(),
        };
        // SAFETY: ptr is a valid, aligned u32 for the duration of the call.
        let _rc = unsafe {
            libc::syscall(
                libc::SYS_futex,
                ptr.as_ptr(),
                libc::FUTEX_WAIT | libc::FUTEX_PRIVATE_FLAG,
                expected,
                ts,
            )
        };
        // SUCCESS: notified by wake; EINTR: caller's responsibility to
        // retry; EAGAIN: ptr.* != expected; EINVAL: possibly timeout
        // overflow; ETIMEDOUT; anything else is a spurious wakeup and the
        // caller retries.
        return;
    }

    #[cfg(target_vendor = "apple")]
    {
        let us: u32 = match timeout_ns {
            None => 0, // 0 means infinite
            Some(ns) => {
                let us = (ns / 1_000).min(u32::MAX as u64) as u32;
                if us == 0 {
                    1
                } else {
                    us
                }
            }
        };
        // SAFETY: ptr is valid for the duration of the call.
        let _status = unsafe {
            __ulock_wait(
                UL_COMPARE_AND_WAIT | ULF_NO_ERRNO,
                ptr.as_ptr().cast(),
                expected as u64,
                us,
            )
        };
        // Negative statuses (EINTR, EFAULT, ETIMEDOUT, ...) are all
        // spurious wakeups; the caller retries.
        return;
    }

    #[cfg(target_os = "freebsd")]
    {
        let mut tm_size: usize = 0;
        let mut tm = UmtxTime {
            timeout: libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            },
            flags: 0, // relative time
            clockid: libc::CLOCK_MONOTONIC as u32,
        };
        let mut tm_ptr: *mut UmtxTime = std::ptr::null_mut();
        if let Some(ns) = timeout_ns {
            tm_ptr = &mut tm;
            tm_size = std::mem::size_of::<UmtxTime>();
            tm.timeout = libc::timespec {
                tv_sec: (ns / 1_000_000_000) as libc::time_t,
                tv_nsec: (ns % 1_000_000_000) as libc::c_long,
            };
        }
        // SAFETY: valid futex word and optional timeout struct.
        unsafe {
            _umtx_op(
                ptr.as_ptr().cast(),
                UMTX_OP_WAIT_UINT_PRIVATE,
                expected as libc::c_ulong,
                tm_size as *mut libc::c_void,
                tm_ptr.cast(),
            )
        };
        return;
    }

    #[allow(unreachable_code)]
    {
        // Portable fallback: futex waits may wake spuriously, so a
        // bounded sleep is a valid (if inefficient) implementation.
        // Contention is not expected in libghostty-vt's threading
        // model, so this is effectively never reached.
        if ptr.load(Ordering::SeqCst) != expected {
            return;
        }
        let ns = timeout_ns.unwrap_or(1_000_000).min(1_000_000);
        let ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: ns as libc::c_long,
        };
        // SAFETY: valid timespec.
        unsafe { libc::nanosleep(&ts, std::ptr::null_mut()) };
    }
}

#[cold]
pub fn futex_wake(ptr: &AtomicU32, max_waiters: u32) {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        // SAFETY: ptr is a valid, aligned u32.
        unsafe {
            libc::syscall(
                libc::SYS_futex,
                ptr.as_ptr(),
                libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG,
                max_waiters.min(i32::MAX as u32) as c_int,
            )
        };
        return;
    }

    #[cfg(target_vendor = "apple")]
    {
        let mut flags = UL_COMPARE_AND_WAIT | ULF_NO_ERRNO;
        if max_waiters > 1 {
            flags |= ULF_WAKE_ALL;
        }
        loop {
            // SAFETY: ptr is valid.
            let status = unsafe { __ulock_wake(flags, ptr.as_ptr().cast(), 0) };
            if status >= 0 {
                return;
            }
            match -status {
                libc::EINTR | libc::ECANCELED => continue, // spurious wake
                _ => return,
            }
        }
    }

    #[cfg(target_os = "freebsd")]
    {
        // SAFETY: ptr is valid.
        unsafe {
            _umtx_op(
                ptr.as_ptr().cast(),
                UMTX_OP_WAKE_PRIVATE,
                max_waiters as libc::c_ulong,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        return;
    }

    // Portable fallback waiters poll with a timeout; nothing to do.
    #[allow(unreachable_code)]
    {
        let _ = (ptr, max_waiters);
    }
}
