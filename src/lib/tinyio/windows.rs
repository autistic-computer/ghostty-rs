//! Port of `src/lib/tinyio/windows.zig`.
//!
//! STATUS: ported (best effort; Windows-only, not compiled on Linux)
//!
//! Windows (NT) impl of TinyIo.
//!
//! Modeled on the Windows impl of `std.Io.Threaded`, minus cancelation and
//! the sleep-and-retry loops it wraps around two kernel quirks (TinyIo
//! cannot sleep, so those statuses surface as `FileBusy`). Everything
//! goes through ntdll and kernel32, which static consumers already link.
//! Paths arrive as WTF-8 and are converted to the NT-prefixed WTF-16 form
//! `NtCreateFile` expects, the same way Threaded converts them.

#![cfg(windows)]

use std::sync::atomic::AtomicU32;

use super::{Dir, Error, File, FileFlags, FileKind, Lock, OpenFileOptions, Stat, MAX_IOVECS_LEN};
use crate::os::windows as os_windows;
use crate::os::windows::exp::{kernel32, ntdll};
use crate::os::windows::{HANDLE, LARGE_INTEGER, NTSTATUS};

/// Zig: `std.os.windows.PATH_MAX_WIDE`.
const PATH_MAX_WIDE: usize = 32767;

const NT_PREFIX: [u16; 4] = [b'\\' as u16, b'?' as u16, b'?' as u16, b'\\' as u16];
const UNC_NT_PREFIX: [u16; 8] = [
    b'\\' as u16,
    b'?' as u16,
    b'?' as u16,
    b'\\' as u16,
    b'U' as u16,
    b'N' as u16,
    b'C' as u16,
    b'\\' as u16,
];
const SHARE_ALL: u32 =
    os_windows::FILE_SHARE_READ | os_windows::FILE_SHARE_WRITE | os_windows::FILE_SHARE_DELETE;

/// The current directory handle from the PEB (Zig: `Dir.cwd().handle`).
pub fn cwd_handle() -> HANDLE {
    os_windows::peb_current_directory_handle()
}

/// A WTF-16 path buffer sized for the longest path NT accepts, plus one
/// slot for the terminator the Rtl path functions want.
struct WindowsPath {
    data: Box<[u16; PATH_MAX_WIDE + 1]>,
    len: usize,
}

impl WindowsPath {
    fn new() -> WindowsPath {
        WindowsPath {
            data: vec![0u16; PATH_MAX_WIDE + 1]
                .into_boxed_slice()
                .try_into()
                .unwrap(),
            len: 0,
        }
    }

    fn span(&self) -> &[u16] {
        &self.data[..self.len]
    }

    fn set_len(&mut self, len: usize) {
        self.len = len;
        self.data[len] = 0;
    }

    /// True for absolute NT paths (`\??\...`). These must be opened with
    /// a null root directory; anything else is relative to a handle.
    fn is_nt(&self) -> bool {
        has_common_nt_prefix(self.span())
    }

    /// Rewrites the Win32 path produced by RtlGetFullPathName_U into NT
    /// form in place: `\\?\X` and `\\.\X` become `\??\X`, `\\server\share`
    /// becomes `\??\UNC\server\share`, and `C:\...` gets `\??\` in front.
    fn win32_to_nt(&mut self) -> Result<(), Error> {
        let sep = b'\\' as u16;
        let len = self.len;
        let p = &self.data[..len];
        if p.len() >= 4
            && p[0] == sep
            && p[1] == sep
            && (p[2] == b'?' as u16 || p[2] == b'.' as u16)
            && p[3] == sep
        {
            self.data[..NT_PREFIX.len()].copy_from_slice(&NT_PREFIX);
            return Ok(());
        }
        if p.len() >= 2 && p[0] == sep && p[1] == sep {
            let rest_len = len - 2;
            if UNC_NT_PREFIX.len() + rest_len > PATH_MAX_WIDE {
                return Err(Error::NameTooLong);
            }
            self.data.copy_within(2..len, UNC_NT_PREFIX.len());
            self.data[..UNC_NT_PREFIX.len()].copy_from_slice(&UNC_NT_PREFIX);
            self.set_len(UNC_NT_PREFIX.len() + rest_len);
            return Ok(());
        }
        if NT_PREFIX.len() + len > PATH_MAX_WIDE {
            return Err(Error::NameTooLong);
        }
        self.data.copy_within(0..len, NT_PREFIX.len());
        self.data[..NT_PREFIX.len()].copy_from_slice(&NT_PREFIX);
        self.set_len(NT_PREFIX.len() + len);
        Ok(())
    }
}

/// Zig: `std.os.windows.hasCommonNtPrefix`.
fn has_common_nt_prefix(p: &[u16]) -> bool {
    p.len() >= 4 && p[..4] == NT_PREFIX
}

/// Zig: `std.os.windows.wtf8ToWtf16Le`.
fn wtf8_to_wtf16le(out: &mut [u16], path: &[u8]) -> Result<usize, Error> {
    // WTF-8 is UTF-8 that also allows encoded surrogates; decode manually.
    let mut i = 0;
    let mut n = 0;
    while i < path.len() {
        let b0 = path[i] as u32;
        let (cp, len) = if b0 < 0x80 {
            (b0, 1)
        } else if b0 & 0xE0 == 0xC0 && i + 1 < path.len() {
            (((b0 & 0x1F) << 6) | (path[i + 1] as u32 & 0x3F), 2)
        } else if b0 & 0xF0 == 0xE0 && i + 2 < path.len() {
            (
                ((b0 & 0x0F) << 12)
                    | ((path[i + 1] as u32 & 0x3F) << 6)
                    | (path[i + 2] as u32 & 0x3F),
                3,
            )
        } else if b0 & 0xF8 == 0xF0 && i + 3 < path.len() {
            (
                ((b0 & 0x07) << 18)
                    | ((path[i + 1] as u32 & 0x3F) << 12)
                    | ((path[i + 2] as u32 & 0x3F) << 6)
                    | (path[i + 3] as u32 & 0x3F),
                4,
            )
        } else {
            return Err(Error::BadPathName);
        };
        for k in 1..len {
            if path[i + k] & 0xC0 != 0x80 {
                return Err(Error::BadPathName);
            }
        }
        i += len;
        if cp >= 0x10000 {
            if n + 2 > out.len() {
                return Err(Error::NameTooLong);
            }
            let v = cp - 0x10000;
            out[n] = 0xD800 | (v >> 10) as u16;
            out[n + 1] = 0xDC00 | (v & 0x3FF) as u16;
            n += 2;
        } else {
            if n + 1 > out.len() {
                return Err(Error::NameTooLong);
            }
            out[n] = cp as u16;
            n += 1;
        }
    }
    Ok(n)
}

/// Zig: `std.unicode.wtf16LeToWtf8`. Returns the bytes written or
/// `NameTooLong` if `out` is too small.
fn wtf16le_to_wtf8(out: &mut [u8], path: &[u16]) -> Result<usize, Error> {
    let mut n = 0;
    let mut i = 0;
    while i < path.len() {
        let mut cp = path[i] as u32;
        i += 1;
        if (0xD800..0xDC00).contains(&cp)
            && i < path.len()
            && (0xDC00..0xE000).contains(&(path[i] as u32))
        {
            cp = 0x10000 + ((cp - 0xD800) << 10) + (path[i] as u32 - 0xDC00);
            i += 1;
        }
        let mut buf = [0u8; 4];
        let len = if cp < 0x80 {
            buf[0] = cp as u8;
            1
        } else if cp < 0x800 {
            buf[0] = 0xC0 | (cp >> 6) as u8;
            buf[1] = 0x80 | (cp & 0x3F) as u8;
            2
        } else if cp < 0x10000 {
            buf[0] = 0xE0 | (cp >> 12) as u8;
            buf[1] = 0x80 | ((cp >> 6) & 0x3F) as u8;
            buf[2] = 0x80 | (cp & 0x3F) as u8;
            3
        } else {
            buf[0] = 0xF0 | (cp >> 18) as u8;
            buf[1] = 0x80 | ((cp >> 12) & 0x3F) as u8;
            buf[2] = 0x80 | ((cp >> 6) & 0x3F) as u8;
            buf[3] = 0x80 | (cp & 0x3F) as u8;
            4
        };
        if n + len > out.len() {
            return Err(Error::NameTooLong);
        }
        out[n..n + len].copy_from_slice(&buf[..len]);
        n += len;
    }
    Ok(n)
}

#[derive(PartialEq, Eq)]
enum Win32PathType {
    Relative,
    RootLocalDevice,
    Other,
}

/// Zig: `std.fs.path.getWin32PathType` (only the distinctions we need).
fn get_win32_path_type(p: &[u16]) -> Win32PathType {
    let is_sep = |c: u16| c == b'\\' as u16 || c == b'/' as u16;
    if p.is_empty() {
        return Win32PathType::Relative;
    }
    if is_sep(p[0]) {
        // `\\.` or `\\?` exactly: root local device.
        if p.len() == 3 && is_sep(p[1]) && (p[2] == b'.' as u16 || p[2] == b'?' as u16) {
            return Win32PathType::RootLocalDevice;
        }
        return Win32PathType::Other;
    }
    if p.len() >= 2 && p[1] == b':' as u16 {
        return Win32PathType::Other;
    }
    Win32PathType::Relative
}

/// Zig: `std.os.windows.normalizePath`: collapse `.`/`..` components and
/// convert `/` to `\` in place. Fails if `..` climbs above the start.
fn normalize_path(buf: &mut [u16]) -> Result<usize, ()> {
    let is_sep = |c: u16| c == b'\\' as u16 || c == b'/' as u16;
    let mut components: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < buf.len() {
        while i < buf.len() && is_sep(buf[i]) {
            i += 1;
        }
        let start = i;
        while i < buf.len() && !is_sep(buf[i]) {
            i += 1;
        }
        if start == i {
            break;
        }
        let comp = &buf[start..i];
        if comp == [b'.' as u16] {
            continue;
        }
        if comp == [b'.' as u16, b'.' as u16] {
            if components.pop().is_none() {
                return Err(());
            }
            continue;
        }
        components.push((start, i));
    }
    let mut out: Vec<u16> = Vec::with_capacity(buf.len());
    for (k, (s, e)) in components.iter().enumerate() {
        if k > 0 {
            out.push(b'\\' as u16);
        }
        out.extend_from_slice(&buf[*s..*e]);
    }
    buf[..out.len()].copy_from_slice(&out);
    Ok(out.len())
}

/// Converts a WTF-8 path into what `NtCreateFile` accepts, the way
/// `std.Io.Threaded.sliceToPrefixedFileW` does: NT paths (`\??\...`) pass
/// through, relative paths are normalized and stay relative to `dir`,
/// and everything else is resolved to an absolute `\??\` path through
/// RtlGetFullPathName_U.
fn path_to_nt(dir: Option<HANDLE>, path: &[u8], out: &mut WindowsPath) -> Result<(), Error> {
    let len = wtf8_to_wtf16le(&mut out.data[..PATH_MAX_WIDE], path)?;
    out.set_len(len);
    if out.is_nt() {
        return Ok(());
    }

    let path_type = get_win32_path_type(out.span());
    match path_type {
        Win32PathType::Relative => {
            let len = out.len;
            if let Ok(len) = normalize_path(&mut out.data[..len]) {
                out.set_len(len);
                return Ok(());
            }
            // Escapes the directory; resolved to an absolute path below.
        }
        Win32PathType::RootLocalDevice => {
            // `\\.` and `\\?` are the NT prefix and nothing else.
            out.data[..NT_PREFIX.len()].copy_from_slice(&NT_PREFIX);
            out.set_len(NT_PREFIX.len());
            return Ok(());
        }
        Win32PathType::Other => {}
    }

    // RtlGetFullPathName_U resolves against the process working directory,
    // so a relative path against any other directory handle is anchored to
    // that directory's final path first.
    let mut full = vec![0u16; PATH_MAX_WIDE + 1];
    let mut full_len = 0usize;
    if path_type == Win32PathType::Relative {
        if let Some(dir_handle) = dir {
            if dir_handle != cwd_handle() {
                let mut tmp = vec![0u16; PATH_MAX_WIDE + 1];
                let dir_path = final_path(dir_handle, &mut tmp).map_err(|e| match e {
                    Error::NameTooLong => Error::NameTooLong,
                    _ => Error::Unexpected,
                })?;
                full[..dir_path.len()].copy_from_slice(dir_path);
                full_len = dir_path.len();
                full[full_len] = b'\\' as u16;
                full_len += 1;
            }
        }
    }
    if full_len + out.len > PATH_MAX_WIDE {
        return Err(Error::NameTooLong);
    }
    full[full_len..full_len + out.len].copy_from_slice(out.span());
    full_len += out.len;
    full[full_len] = 0;

    // SAFETY: `full` is NUL-terminated and `out.data` holds PATH_MAX_WIDE
    // u16s.
    let byte_len = unsafe {
        ntdll::RtlGetFullPathName_U(
            full.as_ptr(),
            (PATH_MAX_WIDE * 2) as u32,
            out.data.as_mut_ptr(),
            std::ptr::null_mut(),
        )
    } as usize;
    if byte_len == 0 {
        return Err(Error::BadPathName);
    }
    if byte_len / 2 > PATH_MAX_WIDE {
        return Err(Error::NameTooLong);
    }
    out.set_len(byte_len / 2);
    out.win32_to_nt()
}

/// The canonical Win32 path of an open handle from GetFinalPathNameByHandleW
/// with the `\\?\` prefix removed, which is the form Threaded's realpath
/// returns: `\\?\C:\x` becomes `C:\x` and `\\?\UNC\srv\share\x` becomes
/// `\\srv\share\x`. The result aliases `buf`.
fn final_path(handle: HANDLE, buf: &mut [u16]) -> Result<&[u16], Error> {
    // SAFETY: buf is valid for writes of its length.
    let len = unsafe {
        kernel32::GetFinalPathNameByHandleW(
            handle,
            buf.as_mut_ptr(),
            buf.len() as u32,
            os_windows::FILE_NAME_NORMALIZED | os_windows::VOLUME_NAME_DOS,
        )
    } as usize;
    if len == 0 {
        return Err(match os_windows::get_last_error() {
            os_windows::ERROR_FILE_NOT_FOUND
            | os_windows::ERROR_PATH_NOT_FOUND
            | os_windows::ERROR_INVALID_HANDLE => Error::FileNotFound,
            os_windows::ERROR_ACCESS_DENIED => Error::AccessDenied,
            os_windows::ERROR_NOT_ENOUGH_MEMORY => Error::SystemResources,
            _ => Error::Unexpected,
        });
    }
    // A too-small buffer reports the size needed, terminator included.
    if len > PATH_MAX_WIDE {
        return Err(Error::NameTooLong);
    }

    let q = b'?' as u16;
    let bs = b'\\' as u16;
    if len >= 4 && buf[0] == bs && buf[1] == bs && buf[2] == q && buf[3] == bs {
        // `\\?\` is the Win32 spelling of `\??\`; strip it and fold
        // `UNC\` back into `\\` (Zig: `ntToWin32Namespace`).
        let rest = &buf[4..len];
        let unc = [b'U' as u16, b'N' as u16, b'C' as u16, bs];
        if rest.len() >= 4 && rest[..4].eq_ignore_ascii_case_u16(&unc) {
            let tail_len = len - 8;
            buf.copy_within(8..len, 2);
            buf[0] = bs;
            buf[1] = bs;
            return Ok(&buf[..tail_len + 2]);
        }
        buf.copy_within(4..len, 0);
        return Ok(&buf[..len - 4]);
    }
    Ok(&buf[..len])
}

trait EqIgnoreAsciiCaseU16 {
    fn eq_ignore_ascii_case_u16(&self, other: &[u16]) -> bool;
}

impl EqIgnoreAsciiCaseU16 for [u16] {
    fn eq_ignore_ascii_case_u16(&self, other: &[u16]) -> bool {
        let lower = |c: u16| {
            if (b'A' as u16..=b'Z' as u16).contains(&c) {
                c + 32
            } else {
                c
            }
        };
        self.len() == other.len() && self.iter().zip(other).all(|(a, b)| lower(*a) == lower(*b))
    }
}

fn real_path_handle(handle: HANDLE, out_buffer: &mut [u8]) -> Result<usize, Error> {
    let mut wide = vec![0u16; PATH_MAX_WIDE + 1];
    let path = final_path(handle, &mut wide)?;
    wtf16le_to_wtf8(out_buffer, path)
}

/// Opens an existing file or directory. `path` is relative to `dir`
/// unless it is an absolute NT path, in which case `dir` is ignored
/// (NtCreateFile rejects a root directory paired with an absolute name).
fn open_nt(dir: HANDLE, path: &WindowsPath, access: u32, options: u32) -> Result<HANDLE, Error> {
    let span = path.span();
    let mut name = os_windows::UNICODE_STRING {
        Length: (span.len() * 2) as u16,
        MaximumLength: (span.len() * 2) as u16,
        Buffer: span.as_ptr() as *mut u16,
    };
    let mut attr = os_windows::OBJECT_ATTRIBUTES::new(
        if path.is_nt() {
            std::ptr::null_mut()
        } else {
            dir
        },
        &mut name,
    );
    let mut iosb = os_windows::IO_STATUS_BLOCK::default();
    let mut handle: HANDLE = std::ptr::null_mut();
    // SAFETY: all pointers are valid for the call.
    let status = unsafe {
        ntdll::NtCreateFile(
            &mut handle,
            access,
            &mut attr,
            &mut iosb,
            std::ptr::null_mut(),
            os_windows::FILE_ATTRIBUTE_NORMAL,
            SHARE_ALL,
            os_windows::FILE_OPEN,
            options,
            std::ptr::null_mut(),
            0,
        )
    };
    use os_windows::status as s;
    match status {
        s::SUCCESS => Ok(handle),
        s::OBJECT_NAME_INVALID | s::OBJECT_PATH_SYNTAX_BAD => Err(Error::BadPathName),
        s::OBJECT_NAME_NOT_FOUND | s::OBJECT_PATH_NOT_FOUND => Err(Error::FileNotFound),
        s::BAD_NETWORK_PATH | s::BAD_NETWORK_NAME => Err(Error::NetworkNotFound),
        s::NO_MEDIA_IN_DEVICE | s::PIPE_NOT_AVAILABLE => Err(Error::NoDevice),
        s::ACCESS_DENIED | s::USER_MAPPED_FILE => Err(Error::AccessDenied),
        s::PIPE_BUSY => Err(Error::PipeBusy),
        s::OBJECT_NAME_COLLISION => Err(Error::PathAlreadyExists),
        s::FILE_IS_A_DIRECTORY => Err(Error::IsDir),
        s::NOT_A_DIRECTORY => Err(Error::NotDir),
        s::VIRUS_INFECTED | s::VIRUS_DELETED => Err(Error::AntivirusInterference),
        // Threaded sleeps and retries these (a kernel bug with recently
        // closed executables, and deletes still in progress).
        s::SHARING_VIOLATION | s::DELETE_PENDING => Err(Error::FileBusy),
        other => Err(os_windows::unexpected_status(other)),
    }
}

pub fn dir_open_file(dir: Dir, sub_path: &[u8], options: OpenFileOptions) -> Result<File, Error> {
    // Same as the POSIX impl: nothing in the terminal locks files.
    if options.lock != Lock::None {
        return Err(Error::FileLocksUnsupported);
    }
    let mut path = WindowsPath::new();
    path_to_nt(Some(dir.handle), sub_path, &mut path)?;

    // Directories can never be opened for writing, and `.` and `..`
    // always name a directory.
    let allow_directory = options.allow_directory && !options.is_write();
    if !allow_directory
        && (path.span() == [b'.' as u16] || path.span() == [b'.' as u16, b'.' as u16])
    {
        return Err(Error::IsDir);
    }

    let mut flags: u32 = os_windows::FILE_SYNCHRONOUS_IO_NONALERT;
    if !allow_directory {
        flags |= os_windows::FILE_NON_DIRECTORY_FILE;
    }
    if !options.follow_symlinks {
        flags |= os_windows::FILE_OPEN_REPARSE_POINT;
    }
    let mut access = os_windows::SYNCHRONIZE;
    if options.is_read() {
        access |= os_windows::GENERIC_READ;
    }
    if options.is_write() {
        access |= os_windows::GENERIC_WRITE;
    }
    let handle = open_nt(dir.handle, &path, access, flags)?;
    Ok(File {
        handle,
        flags: FileFlags { nonblocking: false },
    })
}

pub fn dir_close(dirs: &[Dir]) {
    for dir in dirs {
        // SAFETY: closing a handle.
        unsafe { ntdll::NtClose(dir.handle) };
    }
}

pub fn file_close(files: &[File]) {
    for file in files {
        // SAFETY: closing a handle.
        unsafe { ntdll::NtClose(file.handle) };
    }
}

/// Zig: `std.os.windows.fromSysTime`: 100ns intervals since 1601 to
/// nanoseconds since the Unix epoch.
fn from_sys_time(hns: i64) -> i128 {
    const EPOCH_ADJ: i128 = 11_644_473_600 * 1_000_000_000;
    hns as i128 * 100 - EPOCH_ADJ
}

pub fn file_stat(file: File) -> Result<Stat, Error> {
    let mut iosb = os_windows::IO_STATUS_BLOCK::default();
    let mut info: os_windows::FILE_ALL_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: info is valid for writes of its size.
    let status = unsafe {
        ntdll::NtQueryInformationFile(
            file.handle,
            &mut iosb,
            (&mut info as *mut os_windows::FILE_ALL_INFORMATION).cast(),
            std::mem::size_of::<os_windows::FILE_ALL_INFORMATION>() as u32,
            os_windows::FILE_INFORMATION_CLASS_ALL,
        )
    };
    match status {
        // The trailing name is variable length and unused, so an
        // overflow only means it was truncated.
        os_windows::status::SUCCESS | os_windows::status::BUFFER_OVERFLOW => {}
        os_windows::status::ACCESS_DENIED => return Err(Error::AccessDenied),
        other => return Err(os_windows::unexpected_status(other)),
    }

    let attrs = info.BasicInformation.FileAttributes;
    let kind = if attrs & os_windows::FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        let mut tag: os_windows::FILE_ATTRIBUTE_TAG_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: tag is valid for writes of its size.
        let status = unsafe {
            ntdll::NtQueryInformationFile(
                file.handle,
                &mut iosb,
                (&mut tag as *mut os_windows::FILE_ATTRIBUTE_TAG_INFORMATION).cast(),
                std::mem::size_of::<os_windows::FILE_ATTRIBUTE_TAG_INFORMATION>() as u32,
                os_windows::FILE_INFORMATION_CLASS_ATTRIBUTE_TAG,
            )
        };
        match status {
            os_windows::status::SUCCESS => {}
            os_windows::status::ACCESS_DENIED => return Err(Error::AccessDenied),
            other => return Err(os_windows::unexpected_status(other)),
        }
        // IsReparseTagNameSurrogate: bit 29.
        if tag.ReparseTag & 0x2000_0000 != 0 {
            FileKind::SymLink
        } else {
            FileKind::Unknown
        }
    } else if attrs & os_windows::FILE_ATTRIBUTE_DIRECTORY != 0 {
        FileKind::Directory
    } else {
        FileKind::File
    };

    Ok(Stat {
        inode: info.InternalInformation.IndexNumber as u64,
        size: info.StandardInformation.EndOfFile as u64,
        permissions: 0o666,
        kind,
        atime: from_sys_time(info.BasicInformation.LastAccessTime),
        mtime: from_sys_time(info.BasicInformation.LastWriteTime),
        ctime: from_sys_time(info.BasicInformation.ChangeTime),
        nlink: info.StandardInformation.NumberOfLinks as u64,
        block_size: 4096,
    })
}

pub fn file_length(file: File) -> Result<u64, Error> {
    let mut iosb = os_windows::IO_STATUS_BLOCK::default();
    let mut info: os_windows::FILE_STANDARD_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: info is valid for writes of its size.
    let status = unsafe {
        ntdll::NtQueryInformationFile(
            file.handle,
            &mut iosb,
            (&mut info as *mut os_windows::FILE_STANDARD_INFORMATION).cast(),
            std::mem::size_of::<os_windows::FILE_STANDARD_INFORMATION>() as u32,
            os_windows::FILE_INFORMATION_CLASS_STANDARD,
        )
    };
    match status {
        os_windows::status::SUCCESS => Ok(info.EndOfFile as u64),
        os_windows::status::ACCESS_DENIED => Err(Error::AccessDenied),
        other => Err(os_windows::unexpected_status(other)),
    }
}

/// One NtReadFile into one buffer. A positional read supplies an explicit
/// byte offset; a streaming read uses and advances the handle's own file
/// position (TinyIo only opens synchronous handles, which track it).
fn nt_read(
    positional: bool,
    handle: HANDLE,
    buffer: &mut [u8],
    offset: u64,
) -> Result<usize, Error> {
    let mut iosb = os_windows::IO_STATUS_BLOCK::default();
    let signed_offset: LARGE_INTEGER;
    let offset_ptr: *const LARGE_INTEGER = if positional {
        signed_offset = i64::try_from(offset).map_err(|_| Error::Unseekable)?;
        &signed_offset
    } else {
        std::ptr::null()
    };
    // SAFETY: buffer is valid for writes of the given length.
    let status = unsafe {
        ntdll::NtReadFile(
            handle,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null_mut(),
            &mut iosb,
            buffer.as_mut_ptr().cast(),
            buffer.len().min(u32::MAX as usize) as u32,
            offset_ptr,
            std::ptr::null(),
        )
    };
    use os_windows::status as s;
    match status {
        s::SUCCESS => Ok(iosb.Information),
        s::END_OF_FILE | s::PIPE_BROKEN => Err(Error::EndOfStream),
        s::INVALID_HANDLE => Err(Error::NotOpenForReading),
        s::INVALID_DEVICE_REQUEST => Err(Error::IsDir),
        s::FILE_LOCK_CONFLICT => Err(Error::LockViolation),
        s::ACCESS_DENIED => Err(Error::AccessDenied),
        // Pipes and devices reject explicit offsets.
        s::INVALID_PARAMETER if positional => Err(Error::Unseekable),
        // Only asynchronous handles complete later, and TinyIo never
        // creates one; returning here would hand the kernel a dead buffer.
        s::PENDING => unreachable!(),
        other => Err(os_windows::unexpected_status(other)),
    }
}

pub fn file_read_positional(
    file: File,
    data: &mut [&mut [u8]],
    offset: u64,
) -> Result<usize, Error> {
    // NtReadFile takes one buffer, so a scatter read is issued one buffer
    // at a time, bounded like the POSIX iovec path, stopping at the first
    // short read.
    let mut total = 0usize;
    let mut count = 0usize;
    for buffer in data.iter_mut() {
        if buffer.is_empty() {
            continue;
        }
        if count == MAX_IOVECS_LEN {
            break;
        }
        count += 1;
        let n = match nt_read(true, file.handle, buffer, offset + total as u64) {
            Ok(n) => n,
            Err(Error::EndOfStream) => break,
            Err(e) => return Err(e),
        };
        total += n;
        if n < buffer.len() {
            break;
        }
    }
    Ok(total)
}

pub fn file_read_streaming(file: File, data: &mut [&mut [u8]]) -> Result<usize, Error> {
    // Like Threaded, a streaming read fills one buffer per call: a second
    // NtReadFile could block on a pipe that already delivered data.
    for buffer in data.iter_mut() {
        if buffer.is_empty() {
            continue;
        }
        return nt_read(false, file.handle, buffer, 0);
    }
    Ok(0)
}

fn set_file_position(
    handle: HANDLE,
    info: &mut os_windows::FILE_POSITION_INFORMATION,
) -> Result<(), Error> {
    let mut iosb = os_windows::IO_STATUS_BLOCK::default();
    // SAFETY: info is valid for reads of its size.
    let status = unsafe {
        ntdll::NtSetInformationFile(
            handle,
            &mut iosb,
            (info as *mut os_windows::FILE_POSITION_INFORMATION).cast(),
            std::mem::size_of::<os_windows::FILE_POSITION_INFORMATION>() as u32,
            os_windows::FILE_INFORMATION_CLASS_POSITION,
        )
    };
    use os_windows::status as s;
    match status {
        s::SUCCESS => Ok(()),
        s::ACCESS_DENIED => Err(Error::AccessDenied),
        s::PIPE_NOT_AVAILABLE | s::INVALID_PARAMETER | s::INVALID_DEVICE_REQUEST => {
            Err(Error::Unseekable)
        }
        other => Err(os_windows::unexpected_status(other)),
    }
}

pub fn file_seek_by(file: File, offset: i64) -> Result<(), Error> {
    let mut iosb = os_windows::IO_STATUS_BLOCK::default();
    let mut info = os_windows::FILE_POSITION_INFORMATION {
        CurrentByteOffset: 0,
    };
    // SAFETY: info is valid for writes of its size.
    let status = unsafe {
        ntdll::NtQueryInformationFile(
            file.handle,
            &mut iosb,
            (&mut info as *mut os_windows::FILE_POSITION_INFORMATION).cast(),
            std::mem::size_of::<os_windows::FILE_POSITION_INFORMATION>() as u32,
            os_windows::FILE_INFORMATION_CLASS_POSITION,
        )
    };
    use os_windows::status as s;
    match status {
        s::SUCCESS => {}
        s::ACCESS_DENIED => return Err(Error::AccessDenied),
        s::PIPE_NOT_AVAILABLE | s::INVALID_PARAMETER | s::INVALID_DEVICE_REQUEST => {
            return Err(Error::Unseekable)
        }
        other => return Err(os_windows::unexpected_status(other)),
    }
    let current = info.CurrentByteOffset as u64;
    let target = if offset >= 0 {
        current.checked_add(offset as u64)
    } else {
        current.checked_sub(offset.unsigned_abs())
    };
    info.CurrentByteOffset = target.ok_or(Error::Unseekable)? as i64;
    set_file_position(file.handle, &mut info)
}

pub fn file_seek_to(file: File, offset: u64) -> Result<(), Error> {
    let mut info = os_windows::FILE_POSITION_INFORMATION {
        CurrentByteOffset: offset as i64,
    };
    set_file_position(file.handle, &mut info)
}

pub fn file_real_path(file: File, out_buffer: &mut [u8]) -> Result<usize, Error> {
    real_path_handle(file.handle, out_buffer)
}

pub fn dir_real_path_file(
    dir: Dir,
    sub_path: &[u8],
    out_buffer: &mut [u8],
) -> Result<usize, Error> {
    let mut path = WindowsPath::new();
    path_to_nt(Some(dir.handle), sub_path, &mut path)?;
    let handle = open_nt(
        dir.handle,
        &path,
        os_windows::SYNCHRONIZE | os_windows::GENERIC_READ,
        os_windows::FILE_SYNCHRONOUS_IO_NONALERT,
    )?;

    let result = real_path_handle(handle, out_buffer);
    // SAFETY: closing our handle.
    unsafe { ntdll::NtClose(handle) };
    result
}

pub fn dir_delete_file(dir: Dir, sub_path: &[u8]) -> Result<(), Error> {
    // The parent cannot be removed through a handle inside it.
    if sub_path == b".." {
        return Err(Error::FileBusy);
    }
    let mut path = WindowsPath::new();
    path_to_nt(Some(dir.handle), sub_path, &mut path)?;
    // NT has no `.`, but an empty name reopens `dir` itself.
    if sub_path == b"." {
        path.set_len(0);
    }

    let handle = match open_nt(
        dir.handle,
        &path,
        os_windows::DELETE | os_windows::SYNCHRONIZE,
        os_windows::FILE_SYNCHRONOUS_IO_NONALERT
            | os_windows::FILE_NON_DIRECTORY_FILE
            | os_windows::FILE_OPEN_REPARSE_POINT,
    ) {
        Ok(h) => h,
        // Not produced by a plain file open, and not in DeleteFileError.
        Err(
            Error::PipeBusy
            | Error::NoDevice
            | Error::PathAlreadyExists
            | Error::AntivirusInterference,
        ) => return Err(Error::Unexpected),
        Err(e) => return Err(e),
    };

    // Prefer POSIX delete semantics (the name is gone immediately, even
    // with other handles open) and fall back to delete-on-close where the
    // kernel or file system does not support them.
    let mut iosb = os_windows::IO_STATUS_BLOCK::default();
    let mut ex = os_windows::FILE_DISPOSITION_INFORMATION_EX {
        Flags: os_windows::FILE_DISPOSITION_DELETE
            | os_windows::FILE_DISPOSITION_POSIX_SEMANTICS
            | os_windows::FILE_DISPOSITION_IGNORE_READONLY_ATTRIBUTE,
    };
    use os_windows::status as s;
    // SAFETY: ex is valid for reads of its size.
    let mut status: NTSTATUS = unsafe {
        ntdll::NtSetInformationFile(
            handle,
            &mut iosb,
            (&mut ex as *mut os_windows::FILE_DISPOSITION_INFORMATION_EX).cast(),
            std::mem::size_of::<os_windows::FILE_DISPOSITION_INFORMATION_EX>() as u32,
            os_windows::FILE_INFORMATION_CLASS_DISPOSITION_EX,
        )
    };
    if matches!(
        status,
        s::INVALID_PARAMETER | s::INVALID_INFO_CLASS | s::NOT_SUPPORTED
    ) {
        let mut info = os_windows::FILE_DISPOSITION_INFORMATION { DeleteFile: 1 };
        // SAFETY: info is valid for reads of its size.
        status = unsafe {
            ntdll::NtSetInformationFile(
                handle,
                &mut iosb,
                (&mut info as *mut os_windows::FILE_DISPOSITION_INFORMATION).cast(),
                std::mem::size_of::<os_windows::FILE_DISPOSITION_INFORMATION>() as u32,
                os_windows::FILE_INFORMATION_CLASS_DISPOSITION,
            )
        };
    }
    // SAFETY: closing our handle.
    unsafe { ntdll::NtClose(handle) };
    match status {
        s::SUCCESS => Ok(()),
        s::CANNOT_DELETE | s::MEDIA_WRITE_PROTECTED | s::ACCESS_DENIED => Err(Error::AccessDenied),
        other => Err(os_windows::unexpected_status(other)),
    }
}

pub fn random_secure(buffer: &mut [u8]) -> Result<(), Error> {
    // Read the kernel CSPRNG device directly, which is what ProcessPrng
    // and BCryptGenRandom draw from. TinyIo is stateless, so the device is
    // opened per call: three syscalls, fine for one-time password
    // generation, and no bcryptprimitives or advapi32 import.
    let device: Vec<u16> = "\\Device\\CNG".encode_utf16().collect();
    let mut name = os_windows::UNICODE_STRING {
        Length: (device.len() * 2) as u16,
        MaximumLength: (device.len() * 2) as u16,
        Buffer: device.as_ptr() as *mut u16,
    };
    let mut attr = os_windows::OBJECT_ATTRIBUTES::new(std::ptr::null_mut(), &mut name);
    let mut iosb = os_windows::IO_STATUS_BLOCK::default();
    let mut handle: HANDLE = std::ptr::null_mut();
    // SAFETY: valid pointers.
    let status = unsafe {
        ntdll::NtOpenFile(
            &mut handle,
            os_windows::SYNCHRONIZE | os_windows::FILE_READ_DATA,
            &mut attr,
            &mut iosb,
            SHARE_ALL,
            os_windows::FILE_SYNCHRONOUS_IO_NONALERT,
        )
    };
    if status != os_windows::status::SUCCESS {
        return Err(Error::EntropyUnavailable);
    }

    let mut result = Ok(());
    let mut i = 0usize;
    while i < buffer.len() {
        let len = (buffer.len() - i).min(u32::MAX as usize) as u32;
        // SAFETY: the output buffer is valid for `len` bytes.
        let status = unsafe {
            ntdll::NtDeviceIoControlFile(
                handle,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null_mut(),
                &mut iosb,
                os_windows::IOCTL_KSEC_GEN_RANDOM,
                std::ptr::null(),
                0,
                buffer[i..].as_mut_ptr().cast(),
                len,
            )
        };
        if status != os_windows::status::SUCCESS {
            result = Err(Error::EntropyUnavailable);
            break;
        }
        i += len as usize;
    }
    // SAFETY: closing our handle.
    unsafe { ntdll::NtClose(handle) };
    result
}

#[cold]
pub fn futex_wait_inner(ptr: &AtomicU32, expected: u32, timeout_ns: Option<u64>) {
    // RtlWaitOnAddress is what kernel32's WaitOnAddress forwards to;
    // going through ntdll keeps static consumers off synchronization.lib.
    // A negative timeout is relative in 100ns units and null waits
    // forever. Either way the caller re-checks its condition.
    let interval: LARGE_INTEGER;
    let timeout: *const LARGE_INTEGER = match timeout_ns {
        Some(ns) => {
            interval = -((ns / 100).min(i64::MAX as u64) as i64);
            &interval
        }
        None => std::ptr::null(),
    };
    // SAFETY: valid futex word and comparand.
    unsafe {
        ntdll::RtlWaitOnAddress(
            ptr.as_ptr().cast(),
            (&expected as *const u32).cast(),
            std::mem::size_of::<u32>(),
            timeout,
        )
    };
}

#[cold]
pub fn futex_wake(ptr: &AtomicU32, max_waiters: u32) {
    // SAFETY: valid futex word.
    unsafe {
        if max_waiters > 1 {
            ntdll::RtlWakeAddressAll(ptr.as_ptr().cast())
        } else {
            ntdll::RtlWakeAddressSingle(ptr.as_ptr().cast())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn windows_path_conversion() {
        let cwd = cwd_handle();

        // Absolute Win32 paths are canonicalized and NT-prefixed, with
        // either separator and `..` resolved.
        let mut p = WindowsPath::new();
        path_to_nt(Some(cwd), b"C:/foo/../bar", &mut p).unwrap();
        assert_eq!(l("\\??\\C:\\bar"), p.span());
        path_to_nt(Some(cwd), b"\\\\server\\share\\x\\..\\y", &mut p).unwrap();
        assert_eq!(l("\\??\\UNC\\server\\share\\y"), p.span());
        path_to_nt(Some(cwd), b"\\\\?\\C:\\x", &mut p).unwrap();
        assert_eq!(l("\\??\\C:\\x"), p.span());
        path_to_nt(Some(cwd), b"\\\\.\\pipe\\x", &mut p).unwrap();
        assert_eq!(l("\\??\\pipe\\x"), p.span());

        // NT paths pass through untouched.
        path_to_nt(Some(cwd), b"\\??\\C:\\x", &mut p).unwrap();
        assert_eq!(l("\\??\\C:\\x"), p.span());

        // Relative paths stay relative to the directory handle, normalized.
        path_to_nt(Some(cwd), b"a/../b\\c", &mut p).unwrap();
        assert_eq!(l("b\\c"), p.span());
        assert!(!p.is_nt());

        // Relative paths that climb out of the directory resolve against
        // the working directory for cwd.
        path_to_nt(Some(cwd), b"..\\up.txt", &mut p).unwrap();
        assert!(p.is_nt());
        assert!(p.span().ends_with(&l("\\up.txt")));

        // Encoding errors are path errors.
        assert_eq!(
            Err(Error::BadPathName),
            path_to_nt(Some(cwd), b"bad\xff", &mut p)
        );
        let long = vec![b'a'; PATH_MAX_WIDE + 1];
        assert_eq!(
            Err(Error::NameTooLong),
            path_to_nt(Some(cwd), &long, &mut p)
        );
    }
    #[test]
    fn windows_real_path_resolves_through_symlinks() {
        use crate::lib::tinyio::{OpenFileOptions, TinyIo};
        use std::os::windows::io::AsRawHandle;

        let test_io = TinyIo::INIT.io();
        let tmp = std::env::temp_dir().join(format!("ghostty-tinyio-win-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("target.txt"), b"target").unwrap();
        // Needs SeCreateSymbolicLinkPrivilege, which plain users and CI lack.
        if std::os::windows::fs::symlink_file("target.txt", tmp.join("link.txt")).is_err() {
            let _ = std::fs::remove_dir_all(&tmp);
            return;
        }
        let dir_handle = std::fs::File::open(&tmp).unwrap();
        let dir = Dir {
            handle: dir_handle.as_raw_handle() as HANDLE,
        };

        // Following the link (the default) opens the target and realPath
        // resolves to it, which is what the Kitty path validation relies on.
        let file = dir
            .open_file(test_io, b"link.txt", OpenFileOptions::default())
            .unwrap();
        assert_eq!(b"target".len() as u64, file.length(test_io).unwrap());
        assert_eq!(FileKind::File, file.stat(test_io).unwrap().kind);
        let mut path_buf = vec![0u8; 4096];
        let len = file.real_path(test_io, &mut path_buf).unwrap();
        let path = path_buf[..len].to_vec();
        assert!(path.ends_with(b"target.txt"));
        file.close(test_io);

        // Opening the link itself reports a symlink.
        let link = dir
            .open_file(
                test_io,
                b"link.txt",
                OpenFileOptions {
                    follow_symlinks: false,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(FileKind::SymLink, link.stat(test_io).unwrap().kind);
        link.close(test_io);

        // dirRealPathFile through the link resolves the target too.
        let mut path_buf2 = vec![0u8; 4096];
        let len2 = dir
            .real_path_file(test_io, b"link.txt", &mut path_buf2)
            .unwrap();
        assert_eq!(path, &path_buf2[..len2]);
        drop(dir_handle);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
