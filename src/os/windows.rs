//! Port of `src/os/windows.zig`.
//!
//! STATUS: ported (Windows-only; best effort, not compiled on Linux)
//!
//! NOTE: In Zig this file is a "bridge" between type information (mostly
//! coming from Zig's stdlib) and manually-defined constants and external
//! functions. Rust has no Windows bindings in `std`, so every type and
//! constant is defined here by hand (instead of depending on
//! `windows-sys`).

#![cfg(windows)]
#![allow(non_camel_case_types, non_snake_case, clippy::upper_case_acronyms)]

use std::ffi::c_void;

// Primitive types
pub type BOOL = i32;
pub type DWORD = u32;
pub type DWORD_PTR = usize;
pub type HANDLE = *mut c_void;
pub type HINSTANCE = *mut c_void;
pub type HPCON = *mut c_void;
pub type HRESULT = i32;
pub type LARGE_INTEGER = i64;
pub type LPCWSTR = *const u16;
pub type LPSTR = *mut u8;
pub type LPVOID = *mut c_void;
pub type LPWSTR = *mut u16;
pub type PVOID = *mut c_void;
pub type SIZE_T = usize;
pub type UINT = u32;
pub type ULONG = u32;
pub type ULONG_PTR = usize;
pub type NTSTATUS = i32;
pub type ACCESS_MASK = u32;
pub type CTL_CODE = u32;
pub type FILE_INFORMATION_CLASS = u32;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct COORD {
    pub X: i16,
    pub Y: i16,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct UNICODE_STRING {
    pub Length: u16,
    pub MaximumLength: u16,
    pub Buffer: *mut u16,
}

// Structs and opaque types
pub type LPPROC_THREAD_ATTRIBUTE_LIST = *mut c_void;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SECURITY_ATTRIBUTES {
    pub nLength: DWORD,
    pub lpSecurityDescriptor: LPVOID,
    pub bInheritHandle: BOOL,
}

pub const STARTF_USESTDHANDLES: DWORD = 0x0000_0100;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct STARTUPINFOW {
    pub cb: DWORD,
    pub lpReserved: LPWSTR,
    pub lpDesktop: LPWSTR,
    pub lpTitle: LPWSTR,
    pub dwX: DWORD,
    pub dwY: DWORD,
    pub dwXSize: DWORD,
    pub dwYSize: DWORD,
    pub dwXCountChars: DWORD,
    pub dwYCountChars: DWORD,
    pub dwFillAttribute: DWORD,
    pub dwFlags: DWORD,
    pub wShowWindow: u16,
    pub cbReserved2: u16,
    pub lpReserved2: *mut u8,
    pub hStdInput: HANDLE,
    pub hStdOutput: HANDLE,
    pub hStdError: HANDLE,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct OVERLAPPED_OFFSETS {
    pub Offset: DWORD,
    pub OffsetHigh: DWORD,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union OVERLAPPED_UNION {
    pub DUMMYSTRUCTNAME: OVERLAPPED_OFFSETS,
    pub Pointer: PVOID,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct OVERLAPPED {
    pub Internal: ULONG_PTR,
    pub InternalHigh: ULONG_PTR,
    pub DUMMYUNIONNAME: OVERLAPPED_UNION,
    pub hEvent: HANDLE,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PROCESS_INFORMATION {
    pub hProcess: HANDLE,
    pub hThread: HANDLE,
    pub dwProcessId: DWORD,
    pub dwThreadId: DWORD,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct STARTUPINFOEX {
    pub StartupInfo: STARTUPINFOW,
    pub lpAttributeList: LPPROC_THREAD_ATTRIBUTE_LIST,
}

// Well-known constant values
pub const INFINITE: DWORD = 4294967295;
pub const INVALID_HANDLE_VALUE: HANDLE = usize::MAX as HANDLE;
pub const MAX_PATH: usize = 260;
pub const FALSE: BOOL = 0;
pub const TRUE: BOOL = 1;

// Bit-field and enum constant values
pub const CREATE_UNICODE_ENVIRONMENT: DWORD = 0x00000400;
pub const DELETE: DWORD = 0x00010000;
pub const ERROR_SUCCESS: DWORD = 0;
pub const EXTENDED_STARTUPINFO_PRESENT: DWORD = 0x00080000;
pub const FILE_ATTRIBUTE_NORMAL: DWORD = 0x80;
pub const FILE_ATTRIBUTE_DIRECTORY: DWORD = 0x10;
pub const FILE_ATTRIBUTE_REPARSE_POINT: DWORD = 0x400;
pub const FILE_FLAG_FIRST_PIPE_INSTANCE: DWORD = 0x00080000;
pub const FILE_FLAG_OVERLAPPED: DWORD = 0x40000000;
/// GetFinalPathNameByHandleW dwFlags: normalized name with a DOS drive
/// letter (both are the zero flag values).
pub const FILE_NAME_NORMALIZED: DWORD = 0x0;
pub const FILE_NON_DIRECTORY_FILE: DWORD = 0x00000040;
/// NtCreateFile CreateDisposition: open an existing file, never create.
/// Distinct from the Win32 CreateFileW value OPEN_EXISTING.
pub const FILE_OPEN: DWORD = 0x00000001;
pub const FILE_OPEN_REPARSE_POINT: DWORD = 0x00200000;
pub const FILE_READ_DATA: DWORD = 0x0001;
pub const FILE_SHARE_DELETE: DWORD = 0x00000004;
pub const FILE_SHARE_READ: DWORD = 0x00000001;
pub const FILE_SHARE_WRITE: DWORD = 0x00000002;
pub const FILE_SYNCHRONOUS_IO_NONALERT: DWORD = 0x00000020;
pub const GENERIC_READ: DWORD = 0x80000000;
pub const GENERIC_WRITE: DWORD = 0x40000000;
pub const HANDLE_FLAG_INHERIT: DWORD = 0x00000001;
pub const MEM_COMMIT: DWORD = 0x1000;
pub const MEM_RELEASE: DWORD = 0x8000;
pub const MEM_RESERVE: DWORD = 0x2000;
pub const OPEN_EXISTING: DWORD = 3; // Known as FILE_OPEN in Windows docs
pub const PAGE_READWRITE: DWORD = 0x04;
pub const PIPE_ACCESS_OUTBOUND: DWORD = 0x00000002;
pub const PIPE_TYPE_BYTE: DWORD = 0x00000000;
pub const PROC_THREAD_ATTRIBUTE_ADDITIVE: DWORD = 0x00040000;
pub const PROC_THREAD_ATTRIBUTE_INPUT: DWORD = 0x00020000;
pub const PROC_THREAD_ATTRIBUTE_NUMBER: DWORD = 0x0000FFFF;
pub const PROC_THREAD_ATTRIBUTE_THREAD: DWORD = 0x00010000;
pub const S_OK: HRESULT = 0;
pub const SYNCHRONIZE: DWORD = 0x00100000;
pub const VOLUME_NAME_DOS: DWORD = 0x0;
pub const WAIT_FAILED: DWORD = 0xFFFFFFFF;

pub const FILE_DISPOSITION_DELETE: u32 = 0x0000_0001;
pub const FILE_DISPOSITION_POSIX_SEMANTICS: u32 = 0x0000_0002;
pub const FILE_DISPOSITION_IGNORE_READONLY_ATTRIBUTE: u32 = 0x0000_0010;

// Win32 error codes used by the port.
pub const ERROR_FILE_NOT_FOUND: DWORD = 2;
pub const ERROR_PATH_NOT_FOUND: DWORD = 3;
pub const ERROR_ACCESS_DENIED: DWORD = 5;
pub const ERROR_INVALID_HANDLE: DWORD = 6;
pub const ERROR_NOT_ENOUGH_MEMORY: DWORD = 8;

/// IOCTL that fills the output buffer with bytes from the kernel CSPRNG
/// behind `\Device\CNG` (what ProcessPrng and BCryptGenRandom draw from).
/// CTL_CODE(FILE_DEVICE_KSEC, 2, METHOD_BUFFERED, FILE_ANY_ACCESS).
pub const IOCTL_KSEC_GEN_RANDOM: CTL_CODE = (0x39 << 16) | (2 << 2);

pub const PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE: DWORD = proc_thread_attribute_value(
    ProcThreadAttributeNumber::PROC_THREAD_ATTRIBUTE_PSEUDO_CONSOLE,
    false,
    true,
    false,
);

// FILE_INFORMATION_CLASS values used by the port.
pub const FILE_INFORMATION_CLASS_DISPOSITION: FILE_INFORMATION_CLASS = 13;
pub const FILE_INFORMATION_CLASS_POSITION: FILE_INFORMATION_CLASS = 14;
pub const FILE_INFORMATION_CLASS_STANDARD: FILE_INFORMATION_CLASS = 5;
pub const FILE_INFORMATION_CLASS_ALL: FILE_INFORMATION_CLASS = 18;
pub const FILE_INFORMATION_CLASS_ATTRIBUTE_TAG: FILE_INFORMATION_CLASS = 35;
pub const FILE_INFORMATION_CLASS_DISPOSITION_EX: FILE_INFORMATION_CLASS = 64;

/// NTSTATUS values used by the port.
pub mod status {
    use super::NTSTATUS;
    pub const SUCCESS: NTSTATUS = 0;
    pub const PENDING: NTSTATUS = 0x0000_0103;
    pub const BUFFER_OVERFLOW: NTSTATUS = 0x8000_0005u32 as i32;
    pub const INVALID_INFO_CLASS: NTSTATUS = 0xC000_0003u32 as i32;
    pub const INVALID_HANDLE: NTSTATUS = 0xC000_0008u32 as i32;
    pub const INVALID_PARAMETER: NTSTATUS = 0xC000_000Du32 as i32;
    pub const INVALID_DEVICE_REQUEST: NTSTATUS = 0xC000_0010u32 as i32;
    pub const END_OF_FILE: NTSTATUS = 0xC000_0011u32 as i32;
    pub const NO_MEDIA_IN_DEVICE: NTSTATUS = 0xC000_0013u32 as i32;
    pub const ACCESS_DENIED: NTSTATUS = 0xC000_0022u32 as i32;
    pub const OBJECT_NAME_INVALID: NTSTATUS = 0xC000_0033u32 as i32;
    pub const OBJECT_NAME_NOT_FOUND: NTSTATUS = 0xC000_0034u32 as i32;
    pub const OBJECT_NAME_COLLISION: NTSTATUS = 0xC000_0035u32 as i32;
    pub const OBJECT_PATH_NOT_FOUND: NTSTATUS = 0xC000_003Au32 as i32;
    pub const OBJECT_PATH_SYNTAX_BAD: NTSTATUS = 0xC000_003Bu32 as i32;
    pub const SHARING_VIOLATION: NTSTATUS = 0xC000_0043u32 as i32;
    pub const FILE_LOCK_CONFLICT: NTSTATUS = 0xC000_0054u32 as i32;
    pub const DELETE_PENDING: NTSTATUS = 0xC000_0056u32 as i32;
    pub const MEDIA_WRITE_PROTECTED: NTSTATUS = 0xC000_00A2u32 as i32;
    pub const PIPE_NOT_AVAILABLE: NTSTATUS = 0xC000_00ACu32 as i32;
    pub const PIPE_BUSY: NTSTATUS = 0xC000_00AEu32 as i32;
    pub const FILE_IS_A_DIRECTORY: NTSTATUS = 0xC000_00BAu32 as i32;
    pub const NOT_SUPPORTED: NTSTATUS = 0xC000_00BBu32 as i32;
    pub const BAD_NETWORK_PATH: NTSTATUS = 0xC000_00BEu32 as i32;
    pub const BAD_NETWORK_NAME: NTSTATUS = 0xC000_00CCu32 as i32;
    pub const NOT_A_DIRECTORY: NTSTATUS = 0xC000_0103u32 as i32;
    pub const CANNOT_DELETE: NTSTATUS = 0xC000_0121u32 as i32;
    pub const PIPE_BROKEN: NTSTATUS = 0xC000_014Bu32 as i32;
    pub const USER_MAPPED_FILE: NTSTATUS = 0xC000_0243u32 as i32;
    pub const VIRUS_INFECTED: NTSTATUS = 0xC000_0906u32 as i32;
    pub const VIRUS_DELETED: NTSTATUS = 0xC000_0907u32 as i32;
}

// Types needed for ntdll calls

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct IO_STATUS_BLOCK {
    pub Status: usize,
    pub Information: usize,
}

impl Default for IO_STATUS_BLOCK {
    fn default() -> Self {
        IO_STATUS_BLOCK {
            Status: 0,
            Information: 0,
        }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct OBJECT_ATTRIBUTES {
    pub Length: ULONG,
    pub RootDirectory: HANDLE,
    pub ObjectName: *mut UNICODE_STRING,
    pub Attributes: ULONG,
    pub SecurityDescriptor: PVOID,
    pub SecurityQualityOfService: PVOID,
}

impl OBJECT_ATTRIBUTES {
    pub fn new(root: HANDLE, name: *mut UNICODE_STRING) -> OBJECT_ATTRIBUTES {
        OBJECT_ATTRIBUTES {
            Length: std::mem::size_of::<OBJECT_ATTRIBUTES>() as ULONG,
            RootDirectory: root,
            ObjectName: name,
            Attributes: 0,
            SecurityDescriptor: std::ptr::null_mut(),
            SecurityQualityOfService: std::ptr::null_mut(),
        }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FILE_BASIC_INFORMATION {
    pub CreationTime: LARGE_INTEGER,
    pub LastAccessTime: LARGE_INTEGER,
    pub LastWriteTime: LARGE_INTEGER,
    pub ChangeTime: LARGE_INTEGER,
    pub FileAttributes: ULONG,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FILE_STANDARD_INFORMATION {
    pub AllocationSize: LARGE_INTEGER,
    pub EndOfFile: LARGE_INTEGER,
    pub NumberOfLinks: ULONG,
    pub DeletePending: u8,
    pub Directory: u8,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FILE_INTERNAL_INFORMATION {
    pub IndexNumber: LARGE_INTEGER,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FILE_POSITION_INFORMATION {
    pub CurrentByteOffset: LARGE_INTEGER,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FILE_ALL_INFORMATION {
    pub BasicInformation: FILE_BASIC_INFORMATION,
    pub StandardInformation: FILE_STANDARD_INFORMATION,
    pub InternalInformation: FILE_INTERNAL_INFORMATION,
    pub EaSize: ULONG,
    pub AccessFlags: ACCESS_MASK,
    pub PositionInformation: FILE_POSITION_INFORMATION,
    pub Mode: ULONG,
    pub AlignmentRequirement: ULONG,
    pub FileNameLength: ULONG,
    pub FileName: [u16; 1],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FILE_ATTRIBUTE_TAG_INFORMATION {
    pub FileAttributes: ULONG,
    pub ReparseTag: ULONG,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FILE_DISPOSITION_INFORMATION {
    pub DeleteFile: u8,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FILE_DISPOSITION_INFORMATION_EX {
    pub Flags: ULONG,
}

/// Exported functions by library
pub mod exp {
    pub mod kernel32 {
        use super::super::*;

        #[link(name = "kernel32")]
        extern "system" {
            pub fn CreatePipe(
                hReadPipe: *mut HANDLE,
                hWritePipe: *mut HANDLE,
                lpPipeAttributes: *const SECURITY_ATTRIBUTES,
                nSize: DWORD,
            ) -> BOOL;
            pub fn CreatePseudoConsole(
                size: COORD,
                hInput: HANDLE,
                hOutput: HANDLE,
                dwFlags: DWORD,
                phPC: *mut HPCON,
            ) -> HRESULT;
            pub fn ResizePseudoConsole(hPC: HPCON, size: COORD) -> HRESULT;
            pub fn ClosePseudoConsole(hPC: HPCON);
            pub fn InitializeProcThreadAttributeList(
                lpAttributeList: LPPROC_THREAD_ATTRIBUTE_LIST,
                dwAttributeCount: DWORD,
                dwFlags: DWORD,
                lpSize: *mut SIZE_T,
            ) -> BOOL;
            pub fn UpdateProcThreadAttribute(
                lpAttributeList: LPPROC_THREAD_ATTRIBUTE_LIST,
                dwFlags: DWORD,
                Attribute: DWORD_PTR,
                lpValue: PVOID,
                cbSize: SIZE_T,
                lpPreviousValue: PVOID,
                lpReturnSize: *mut SIZE_T,
            ) -> BOOL;
            pub fn PeekNamedPipe(
                hNamedPipe: HANDLE,
                lpBuffer: LPVOID,
                nBufferSize: DWORD,
                lpBytesRead: *mut DWORD,
                lpTotalBytesAvail: *mut DWORD,
                lpBytesLeftThisMessage: *mut DWORD,
            ) -> BOOL;
            pub fn CreateProcessW(
                lpApplicationName: LPWSTR,
                lpCommandLine: LPWSTR,
                lpProcessAttributes: *mut SECURITY_ATTRIBUTES,
                lpThreadAttributes: *mut SECURITY_ATTRIBUTES,
                bInheritHandles: BOOL,
                dwCreationFlags: DWORD,
                lpEnvironment: *mut std::ffi::c_void,
                lpCurrentDirectory: LPWSTR,
                lpStartupInfo: *mut STARTUPINFOW,
                lpProcessInformation: *mut PROCESS_INFORMATION,
            ) -> BOOL;
            /// https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-getcomputernamea
            pub fn GetComputerNameA(lpBuffer: LPSTR, nSize: *mut DWORD) -> BOOL;
            /// https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-gettemppathw
            pub fn GetTempPathW(nBufferLength: DWORD, lpBuffer: LPWSTR) -> DWORD;
            pub fn SetHandleInformation(hObject: HANDLE, dwMask: DWORD, dwFlags: DWORD) -> BOOL;
            pub fn CreateFileW(
                lpFileName: LPCWSTR,
                dwDesiredAccess: DWORD,
                dwShareMode: DWORD,
                lpSecurityAttributes: *mut SECURITY_ATTRIBUTES,
                dwCreationDisposition: DWORD,
                dwFlagsAndAttributes: DWORD,
                hTemplateFile: HANDLE,
            ) -> HANDLE;
            pub fn CreateNamedPipeW(
                lpName: LPCWSTR,
                dwOpenMode: DWORD,
                dwPipeMode: DWORD,
                nMaxInstances: DWORD,
                nOutBufferSize: DWORD,
                nInBufferSize: DWORD,
                nDefaultTimeOut: DWORD,
                lpSecurityAttributes: *const SECURITY_ATTRIBUTES,
            ) -> HANDLE;
            pub fn CloseHandle(hObject: HANDLE) -> BOOL;
            pub fn VirtualAlloc(
                lpAddress: LPVOID,
                dwSize: SIZE_T,
                flAllocationType: DWORD,
                flProtect: DWORD,
            ) -> LPVOID;
            pub fn VirtualFree(lpAddress: LPVOID, dwSize: SIZE_T, dwFreeType: DWORD) -> BOOL;
            /// https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-discardvirtualmemory
            pub fn DiscardVirtualMemory(VirtualAddress: PVOID, Size: SIZE_T) -> DWORD;
            pub fn WaitForSingleObject(hHandle: HANDLE, dwMilliseconds: DWORD) -> DWORD;
            pub fn GetExitCodeProcess(hProcess: HANDLE, lpExitCode: *mut DWORD) -> BOOL;
            pub fn TerminateProcess(hProcess: HANDLE, uExitCode: UINT) -> BOOL;
            pub fn CancelIoEx(hFile: HANDLE, lpOverlapped: *mut OVERLAPPED) -> BOOL;
            pub fn ReadFile(
                hFile: HANDLE,
                lpBuffer: LPVOID,
                nNumberOfBytesToRead: DWORD,
                lpNumberOfBytesRead: *mut DWORD,
                lpOverlapped: *mut OVERLAPPED,
            ) -> BOOL;
            /// https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-getfinalpathnamebyhandlew
            pub fn GetFinalPathNameByHandleW(
                hFile: HANDLE,
                lpszFilePath: *mut u16,
                cchFilePath: DWORD,
                dwFlags: DWORD,
            ) -> DWORD;
            pub fn GetLastError() -> DWORD;
            pub fn GetCurrentProcessId() -> DWORD;
        }
    }

    pub mod ntdll {
        use super::super::*;

        #[link(name = "ntdll")]
        extern "system" {
            pub fn NtCreateFile(
                FileHandle: *mut HANDLE,
                DesiredAccess: ACCESS_MASK,
                ObjectAttributes: *mut OBJECT_ATTRIBUTES,
                IoStatusBlock: *mut IO_STATUS_BLOCK,
                AllocationSize: *mut LARGE_INTEGER,
                FileAttributes: ULONG,
                ShareAccess: ULONG,
                CreateDisposition: ULONG,
                CreateOptions: ULONG,
                EaBuffer: *mut std::ffi::c_void,
                EaLength: ULONG,
            ) -> NTSTATUS;
            pub fn NtOpenFile(
                FileHandle: *mut HANDLE,
                DesiredAccess: ACCESS_MASK,
                ObjectAttributes: *mut OBJECT_ATTRIBUTES,
                IoStatusBlock: *mut IO_STATUS_BLOCK,
                ShareAccess: ULONG,
                OpenOptions: ULONG,
            ) -> NTSTATUS;
            pub fn NtClose(Handle: HANDLE) -> NTSTATUS;
            pub fn NtReadFile(
                FileHandle: HANDLE,
                Event: HANDLE,
                ApcRoutine: *const std::ffi::c_void,
                ApcContext: *mut std::ffi::c_void,
                IoStatusBlock: *mut IO_STATUS_BLOCK,
                Buffer: *mut std::ffi::c_void,
                Length: ULONG,
                ByteOffset: *const LARGE_INTEGER,
                Key: *const ULONG,
            ) -> NTSTATUS;
            pub fn NtWriteFile(
                FileHandle: HANDLE,
                Event: HANDLE,
                ApcRoutine: *const std::ffi::c_void,
                ApcContext: *mut std::ffi::c_void,
                IoStatusBlock: *mut IO_STATUS_BLOCK,
                Buffer: *const std::ffi::c_void,
                Length: ULONG,
                ByteOffset: *const LARGE_INTEGER,
                Key: *const ULONG,
            ) -> NTSTATUS;
            pub fn NtQueryInformationFile(
                FileHandle: HANDLE,
                IoStatusBlock: *mut IO_STATUS_BLOCK,
                FileInformation: *mut std::ffi::c_void,
                Length: ULONG,
                FileInformationClass: FILE_INFORMATION_CLASS,
            ) -> NTSTATUS;
            pub fn NtSetInformationFile(
                FileHandle: HANDLE,
                IoStatusBlock: *mut IO_STATUS_BLOCK,
                FileInformation: *mut std::ffi::c_void,
                Length: ULONG,
                FileInformationClass: FILE_INFORMATION_CLASS,
            ) -> NTSTATUS;
            pub fn NtDeviceIoControlFile(
                FileHandle: HANDLE,
                Event: HANDLE,
                ApcRoutine: *const std::ffi::c_void,
                ApcContext: *mut std::ffi::c_void,
                IoStatusBlock: *mut IO_STATUS_BLOCK,
                IoControlCode: CTL_CODE,
                InputBuffer: *const std::ffi::c_void,
                InputBufferLength: ULONG,
                OutputBuffer: *mut std::ffi::c_void,
                OutputBufferLength: ULONG,
            ) -> NTSTATUS;
            /// Resolves a Win32 path against the process working directory
            /// and canonicalizes it, keeping any `\\?\` or `\\.\` prefix.
            /// Returns the byte length written excluding the terminator,
            /// the required byte length if the buffer is too small, or 0 on
            /// failure.
            pub fn RtlGetFullPathName_U(
                FileName: *const u16,
                BufferByteLength: ULONG,
                Buffer: *mut u16,
                ShortName: *mut *const u16,
            ) -> ULONG;
            /// The futex primitives that kernel32's WaitOnAddress and
            /// WakeByAddress* forward to. Timeout is a relative (negative)
            /// interval in 100ns units, null to wait forever.
            pub fn RtlWaitOnAddress(
                Address: *const std::ffi::c_void,
                CompareAddress: *const std::ffi::c_void,
                AddressSize: SIZE_T,
                Timeout: *const LARGE_INTEGER,
            ) -> NTSTATUS;
            pub fn RtlWakeAddressSingle(Address: *const std::ffi::c_void);
            pub fn RtlWakeAddressAll(Address: *const std::ffi::c_void);
            pub fn RtlGetCurrentPeb() -> *mut super::super::PEB;
        }
    }
}

/// Partial `RTL_USER_PROCESS_PARAMETERS` (only the fields we read).
#[repr(C)]
pub struct RTL_USER_PROCESS_PARAMETERS {
    pub MaximumLength: ULONG,
    pub Length: ULONG,
    pub Flags: ULONG,
    pub DebugFlags: ULONG,
    pub ConsoleHandle: HANDLE,
    pub ConsoleFlags: ULONG,
    pub hStdInput: HANDLE,
    pub hStdOutput: HANDLE,
    pub hStdError: HANDLE,
    pub CurrentDirectoryPath: UNICODE_STRING,
    pub CurrentDirectoryHandle: HANDLE,
}

/// Partial `PEB` (only the fields we read).
#[repr(C)]
pub struct PEB {
    pub InheritedAddressSpace: u8,
    pub ReadImageFileExecOptions: u8,
    pub BeingDebugged: u8,
    pub BitField: u8,
    pub Mutant: HANDLE,
    pub ImageBaseAddress: PVOID,
    pub Ldr: PVOID,
    pub ProcessParameters: *mut RTL_USER_PROCESS_PARAMETERS,
}

/// Zig: `windows.peb().ProcessParameters.CurrentDirectory.Handle`.
pub fn peb_current_directory_handle() -> HANDLE {
    // SAFETY: the PEB and its process parameters are always valid.
    unsafe { (*(*exp::ntdll::RtlGetCurrentPeb()).ProcessParameters).CurrentDirectoryHandle }
}

/// Zig: `windows.peb().ProcessParameters.hStdError`.
pub fn peb_std_error_handle() -> HANDLE {
    // SAFETY: the PEB and its process parameters are always valid.
    unsafe { (*(*exp::ntdll::RtlGetCurrentPeb()).ProcessParameters).hStdError }
}

// Utility functions

pub fn get_current_process_id() -> DWORD {
    // SAFETY: no preconditions.
    unsafe { exp::kernel32::GetCurrentProcessId() }
}

pub fn get_last_error() -> DWORD {
    // SAFETY: no preconditions.
    unsafe { exp::kernel32::GetLastError() }
}

/// Zig: `windows.unexpectedError`.
pub fn unexpected_error(err: DWORD) -> crate::lib::tinyio::Error {
    log::warn!("unexpected Windows error: {err}");
    crate::lib::tinyio::Error::Unexpected
}

/// Zig: `windows.unexpectedStatus`.
pub fn unexpected_status(status: NTSTATUS) -> crate::lib::tinyio::Error {
    log::warn!("unexpected NTSTATUS: 0x{:08x}", status as u32);
    crate::lib::tinyio::Error::Unexpected
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ProcThreadAttributeNumber {
    PROC_THREAD_ATTRIBUTE_PSEUDO_CONSOLE = 22,
}

/// Corresponds to the ProcThreadAttributeValue define in WinBase.h
pub const fn proc_thread_attribute_value(
    attribute: ProcThreadAttributeNumber,
    thread: bool,
    input: bool,
    additive: bool,
) -> DWORD {
    (attribute as DWORD & PROC_THREAD_ATTRIBUTE_NUMBER)
        | (if thread {
            PROC_THREAD_ATTRIBUTE_THREAD
        } else {
            0
        })
        | (if input {
            PROC_THREAD_ATTRIBUTE_INPUT
        } else {
            0
        })
        | (if additive {
            PROC_THREAD_ATTRIBUTE_ADDITIVE
        } else {
            0
        })
}
