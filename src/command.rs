//! Port of `src/Command.zig`.
//!
//! STATUS: ported
//!
//! Command launches sub-processes. This is an alternate implementation to the
//! Zig std.process.Child since at the time of authoring this, std.process.Child
//! didn't support the options necessary to spawn a shell attached to a pty.
//!
//! Consequently, I didn't implement a lot of features that std.process.Child
//! supports because we didn't need them. Cross-platform subprocessing is not
//! a trivial thing to implement (I've done it in three separate languages now)
//! so if we want to replatform onto std.process.Child I'd love to do that.
//! This was just the fastest way to get something built.
//!
//! Issues with std.process.Child:
//!
//!   * No pre_exec callback for logic after fork but before exec.
//!   * posix_spawn is used for Mac, but doesn't support the necessary
//!     features for tty setup.
//!
//! (Rust port: the same holds for `std::process::Command`, whose `pre_exec`
//! cannot receive the command and which always resolves the program and
//! environment its own way.)
//!
//! # Fork safety (Rust port)
//!
//! Everything the child needs (argv/envp pointer arrays, the `PATH` value,
//! the resource limits to restore) is prepared *before* `fork()`, and the
//! child only performs async-signal-safe calls before `execve` (the Zig code
//! reads `PATH` and the rlimits from the global state in the child; reading
//! them first is equivalent and avoids taking locks after fork). Pre-exec
//! callbacks run in the child and must also restrict themselves to
//! async-signal-safe work; exiting from them uses `_exit` rather than
//! `exit`.

use std::collections::HashMap;
use std::ffi::{c_void, CString};
use std::fmt;

use crate::global;

/// Environment map type (Zig: `std.process.Environ.Map`).
pub type EnvMap = HashMap<String, String>;

/// Function prototype for a function executed /in the child process/ after the
/// fork, but before exec'ing the command. If the function returns a u8, the
/// child process will be exited with that error code.
pub type PreExecFn = fn(&mut Command) -> Option<u8>;

/// Allowable set of errors that can be returned by a post fork function. Any
/// errors will result in the failure to create the surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostForkError {
    PostForkError,
}

impl fmt::Display for PostForkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PostForkError")
    }
}

impl std::error::Error for PostForkError {}

/// Function prototype for a function executed /in the parent process/
/// after the fork.
pub type PostForkFn = fn(&mut Command) -> Result<(), PostForkError>;

/// A file handle for the child's stdio (Zig: `std.Io.File`; only its
/// handle is used).
#[cfg(unix)]
pub type FileHandle = std::os::fd::RawFd;
#[cfg(windows)]
pub type FileHandle = crate::os::windows::HANDLE;

/// The process id type (Zig: `posix.system.pid_t`, which is a process
/// HANDLE on Windows).
#[cfg(unix)]
pub type Pid = libc::pid_t;
#[cfg(windows)]
pub type Pid = crate::os::windows::HANDLE;

/// Errors from [`Command::start`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartError {
    /// Returned /in the child process/ when exec failed. The caller must
    /// exit the child immediately when it sees this.
    ExecFailedInChild,

    /// fork failed with EAGAIN or ENOMEM.
    SystemResources,

    /// The `rt_post_fork` callback failed.
    PostForkError,

    /// A path, argument or environment entry contains a NUL byte (the Zig
    /// API takes NUL-terminated strings and can't express this).
    InvalidArgument,

    /// Any other error.
    Unexpected,
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for StartError {}

impl From<PostForkError> for StartError {
    fn from(_: PostForkError) -> Self {
        StartError::PostForkError
    }
}

/// Errors from [`Command::wait`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitError {
    NoChildProcess,
    InvalidWaitOptions,
    Unexpected,
}

impl fmt::Display for WaitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for WaitError {}

/// Configuration information needed by the apprt pre exec function. Note
/// that this should be a trivially copyable struct and not require any
/// allocation/deallocation.
///
/// Zig: `apprt.runtime.pre_exec.PreExecInfo` if the runtime has a
/// `pre_exec` (the GTK runtime), else an empty struct.
#[cfg(all(feature = "gtk", not(target_os = "macos")))]
pub use crate::apprt::gtk::pre_exec::PreExecInfo as RtPreExecInfo;

/// Configuration information needed by the apprt pre exec function. Note
/// that this should be a trivially copyable struct and not require any
/// allocation/deallocation.
///
/// This runtime has no `pre_exec`, so this is the empty struct Zig uses.
#[cfg(not(all(feature = "gtk", not(target_os = "macos"))))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RtPreExecInfo {}

#[cfg(not(all(feature = "gtk", not(target_os = "macos"))))]
impl RtPreExecInfo {
    pub fn init(_config: &crate::config::Config) -> Self {
        Self {}
    }
}

/// Configuration information needed by the apprt post fork function. Note
/// that this should be a trivially copyable struct and not require any
/// allocation/deallocation.
///
/// Zig: `apprt.runtime.post_fork.PostForkInfo` if the runtime has a
/// `post_fork` (the GTK runtime), else an empty struct.
#[cfg(all(feature = "gtk", not(target_os = "macos")))]
pub use crate::apprt::gtk::post_fork::PostForkInfo as RtPostForkInfo;

/// Configuration information needed by the apprt post fork function. Note
/// that this should be a trivially copyable struct and not require any
/// allocation/deallocation.
///
/// This runtime has no `post_fork`, so this is the empty struct Zig uses.
#[cfg(not(all(feature = "gtk", not(target_os = "macos"))))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RtPostForkInfo {}

#[cfg(not(all(feature = "gtk", not(target_os = "macos"))))]
impl RtPostForkInfo {
    pub fn init(_config: &crate::config::Config) -> Self {
        Self {}
    }
}

/// A value for the `rt_pre_exec_info` field when there is no config to
/// build it from (Zig leaves the field `undefined` in that case). With the
/// GTK runtime this disables the cgroup handling of the hooks.
pub fn rt_pre_exec_info_unset() -> RtPreExecInfo {
    #[cfg(all(feature = "gtk", not(target_os = "macos")))]
    return RtPreExecInfo {
        gtk_single_instance: crate::config::config::GtkSingleInstance::DEFAULT,
        linux_cgroup: crate::config::config::LinuxCgroup::Never,
        linux_cgroup_hard_fail: false,
    };
    #[cfg(not(all(feature = "gtk", not(target_os = "macos"))))]
    RtPreExecInfo::default()
}

/// See [`rt_pre_exec_info_unset`].
pub fn rt_post_fork_info_unset() -> RtPostForkInfo {
    #[cfg(all(feature = "gtk", not(target_os = "macos")))]
    return RtPostForkInfo {
        gtk_single_instance: crate::config::config::GtkSingleInstance::DEFAULT,
        linux_cgroup: crate::config::config::LinuxCgroup::Never,
        linux_cgroup_hard_fail: false,
        linux_cgroup_memory_limit: None,
        linux_cgroup_processes_limit: None,
    };
    #[cfg(not(all(feature = "gtk", not(target_os = "macos"))))]
    RtPostForkInfo::default()
}

/// The runtime's pre exec hook (Zig `apprt.runtime.pre_exec.preExec` if
/// the runtime has one).
pub const RT_PRE_EXEC: Option<PreExecFn> = {
    #[cfg(all(feature = "gtk", not(target_os = "macos")))]
    {
        Some(crate::apprt::gtk::pre_exec::pre_exec)
    }
    #[cfg(not(all(feature = "gtk", not(target_os = "macos"))))]
    {
        None
    }
};

/// The runtime's post fork hook (Zig `apprt.runtime.post_fork.postFork` if
/// the runtime has one).
pub const RT_POST_FORK: Option<PostForkFn> = {
    #[cfg(all(feature = "gtk", not(target_os = "macos")))]
    {
        Some(crate::apprt::gtk::post_fork::post_fork)
    }
    #[cfg(not(all(feature = "gtk", not(target_os = "macos"))))]
    {
        None
    }
};

pub struct Command {
    /// Path to the command to run. This doesn't have to be an absolute path,
    /// because use exec functions that search the PATH, if necessary.
    ///
    /// This field is null-terminated to avoid a copy for the sake of
    /// adding a null terminator since POSIX systems are so common.
    pub path: CString,

    /// Command-line arguments. It is the responsibility of the caller to set
    /// args[0] to the command. If args is empty then args[0] will automatically
    /// be set to equal path.
    pub args: Vec<CString>,

    /// Environment variables for the child process. If this is null, inherits
    /// the environment variables from this process. These are the exact
    /// environment variables to set; these are /not/ merged.
    pub env: Option<EnvMap>,

    /// Working directory to change to in the child process. If not set, the
    /// working directory of the calling process is preserved.
    pub cwd: Option<CString>,

    /// The file handle to set for stdin/out/err. If this isn't set, we do
    /// nothing explicitly so it is up to the behavior of the operating system.
    pub stdin: Option<FileHandle>,
    pub stdout: Option<FileHandle>,
    pub stderr: Option<FileHandle>,

    /// If set, this will be executed /in the child process/ after fork but
    /// before exec. This is useful to setup some state in the child before the
    /// exec process takes over, such as signal handlers, setsid, setuid, etc.
    pub os_pre_exec: Option<PreExecFn>,

    /// If set, this will be executed /in the child process/ after fork but
    /// before exec. This is useful to setup some state in the child before the
    /// exec process takes over, such as signal handlers, setsid, setuid, etc.
    pub rt_pre_exec: Option<PreExecFn>,

    /// Configuration information needed by the apprt pre exec function. Note
    /// that this should be a trivially copyable struct and not require any
    /// allocation/deallocation.
    pub rt_pre_exec_info: RtPreExecInfo,

    /// If set, this will be executed in the /in the parent process/ after the fork.
    pub rt_post_fork: Option<PostForkFn>,

    /// Configuration information needed by the apprt post fork function. Note
    /// that this should be a trivially copyable struct and not require any
    /// allocation/deallocation.
    pub rt_post_fork_info: RtPostForkInfo,

    /// If set, then the process will be created attached to this pseudo console.
    /// `stdin`, `stdout`, and `stderr` will be ignored if set.
    #[cfg(windows)]
    pub pseudo_console: Option<crate::os::windows::HPCON>,

    /// User data that is sent to the callback. Set with setData and getData
    /// for a more user-friendly API.
    pub data: *mut c_void,

    /// Process ID is set after start is called.
    pub pid: Option<Pid>,
}

// SAFETY: `data` is an opaque pointer owned and synchronized by the caller
// (exactly as in Zig); everything else is plain data.
unsafe impl Send for Command {}

impl Default for Command {
    fn default() -> Self {
        Self {
            path: CString::default(),
            args: Vec::new(),
            env: None,
            cwd: None,
            stdin: None,
            stdout: None,
            stderr: None,
            os_pre_exec: None,
            rt_pre_exec: None,
            rt_pre_exec_info: rt_pre_exec_info_unset(),
            rt_post_fork: None,
            rt_post_fork_info: rt_post_fork_info_unset(),
            #[cfg(windows)]
            pseudo_console: None,
            data: std::ptr::null_mut(),
            pid: None,
        }
    }
}

impl fmt::Debug for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Command")
            .field("path", &self.path)
            .field("args", &self.args)
            .field("cwd", &self.cwd)
            .field("pid", &self.pid)
            .finish_non_exhaustive()
    }
}

/// The various methods a process may exit.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// Exited by normal exit call, value is exit status
    Exited(u8),

    /// Exited by a signal, value is the signal
    Signal(u32),

    /// Exited by a stop signal, value is signal
    Stopped(u32),

    /// Unknown exit reason, value is the status from waitpid
    Unknown(u32),
}

#[cfg(unix)]
impl Exit {
    pub fn init(status: u32) -> Exit {
        let status_i = status as libc::c_int;
        if libc::WIFEXITED(status_i) {
            Exit::Exited(libc::WEXITSTATUS(status_i) as u8)
        } else if libc::WIFSIGNALED(status_i) {
            Exit::Signal(libc::WTERMSIG(status_i) as u32)
        } else if libc::WIFSTOPPED(status_i) {
            Exit::Stopped(libc::WSTOPSIG(status_i) as u32)
        } else {
            Exit::Unknown(status)
        }
    }
}

/// The various methods a process may exit.
#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Exited(u32),
}

/// The current process environment block (Zig: `std.c.environ`).
#[cfg(unix)]
fn current_environ() -> *const *const libc::c_char {
    #[cfg(target_vendor = "apple")]
    {
        // SAFETY: _NSGetEnviron always returns a valid pointer.
        unsafe { *libc::_NSGetEnviron() as *const *const libc::c_char }
    }

    #[cfg(not(target_vendor = "apple"))]
    {
        extern "C" {
            static environ: *const *const libc::c_char;
        }
        // SAFETY: reading the libc global.
        unsafe { environ }
    }
}

#[cfg(unix)]
fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

impl Command {
    /// Start the subprocess. This returns immediately once the child is started.
    ///
    /// After this is successful, self.pid is available.
    ///
    /// IMPORTANT: if this returns [`StartError::ExecFailedInChild`] then
    /// the caller *is the forked child* and must exit immediately.
    pub fn start(&mut self) -> Result<(), StartError> {
        // IMPORTANT: do all allocation prior to the fork(). I believe it is undefined
        // behavior if you malloc between fork and exec. The source of the Zig
        // stdlib seems to verify this as well as Go.
        #[cfg(windows)]
        return self.start_windows();

        #[cfg(unix)]
        return self.start_posix();
    }

    #[cfg(unix)]
    fn start_posix(&mut self) -> Result<(), StartError> {
        // Null-terminate all our arguments
        let args_z: Vec<*const libc::c_char> = self
            .args
            .iter()
            .map(|a| a.as_ptr())
            .chain(std::iter::once(std::ptr::null()))
            .collect();

        // Determine our env vars
        let env_storage: Option<Vec<CString>> = match &self.env {
            Some(env_map) => Some(create_null_delimited_env_map(env_map)?),
            None => None,
        };
        let env_ptrs: Option<Vec<*const libc::c_char>> = env_storage.as_ref().map(|v| {
            v.iter()
                .map(|s| s.as_ptr())
                .chain(std::iter::once(std::ptr::null()))
                .collect()
        });
        let envp: *const *const libc::c_char = match &env_ptrs {
            Some(v) => v.as_ptr(),
            None => current_environ(),
        };

        // Prepared before the fork (see module docs).
        let path_env: Vec<u8> = global::environ()
            .get("PATH")
            .map(|s| s.as_bytes().to_vec())
            .unwrap_or_else(|| b"/usr/local/bin:/bin/:/usr/bin".to_vec());
        let rlimits = global::rlimits();

        // Fork.
        let pid = fork()?;

        if pid != 0 {
            // Parent, return immediately.
            self.pid = Some(pid);
            if let Some(f) = self.rt_post_fork {
                f(self)?;
            }
            return Ok(());
        }

        // We are the child.

        // Setup our file descriptors for std streams.
        if let Some(f) = self.stdin {
            if setup_fd(f, libc::STDIN_FILENO).is_err() {
                return Err(StartError::ExecFailedInChild);
            }
        }
        if let Some(f) = self.stdout {
            if setup_fd(f, libc::STDOUT_FILENO).is_err() {
                return Err(StartError::ExecFailedInChild);
            }
        }
        if let Some(f) = self.stderr {
            if setup_fd(f, libc::STDERR_FILENO).is_err() {
                return Err(StartError::ExecFailedInChild);
            }
        }

        // Setup our working directory.
        //
        // NOTE: this can fail if we don't have permission to go to this directory
        // or if due to race conditions it doesn't exist or any various other
        // reasons. We don't want to crash the entire process if this fails so we
        // ignore it. We don't log because that'll show up in the output.
        if let Some(cwd) = &self.cwd {
            // SAFETY: valid C string.
            unsafe { libc::chdir(cwd.as_ptr()) };
        }

        // Restore any rlimits that were set by Ghostty. This might fail but
        // any failures are ignored (its best effort).
        rlimits.restore();

        // If there are pre exec callbacks, call them now.
        if let Some(f) = self.os_pre_exec {
            if let Some(exitcode) = f(self) {
                // SAFETY: terminating the forked child.
                unsafe { libc::_exit(exitcode as libc::c_int) };
            }
        }
        if let Some(f) = self.rt_pre_exec {
            if let Some(exitcode) = f(self) {
                // SAFETY: terminating the forked child.
                unsafe { libc::_exit(exitcode as libc::c_int) };
            }
        }

        let err: i32 = 'execve: {
            // This functionality has been taken from Zig stdlib, a simplified
            // version of the exec bits with PATH search so that we can just
            // offload to execve below.
            let file_slice = self.path.as_bytes();
            if file_slice.contains(&b'/') {
                // SAFETY: valid NUL-terminated argv/envp arrays.
                unsafe { libc::execve(self.path.as_ptr(), args_z.as_ptr(), envp) };
                break 'execve errno();
            }

            let mut path_expanded_buf = [0u8; libc::PATH_MAX as usize];
            let mut err = libc::ENOENT;
            let mut seen_eacces = false;

            for search_path in path_env.split(|&b| b == b':').filter(|p| !p.is_empty()) {
                let path_len = search_path.len() + file_slice.len() + 1;
                if path_expanded_buf.len() < path_len + 1 {
                    break 'execve libc::ENAMETOOLONG;
                }
                path_expanded_buf[..search_path.len()].copy_from_slice(search_path);
                path_expanded_buf[search_path.len()] = b'/';
                path_expanded_buf[search_path.len() + 1..][..file_slice.len()]
                    .copy_from_slice(file_slice);
                path_expanded_buf[path_len] = 0;
                // Replace here, switch on error (any error means that replace
                // failed, but we might need to retry).
                // SAFETY: the buffer is NUL-terminated; argv/envp are valid.
                unsafe { libc::execve(path_expanded_buf.as_ptr().cast(), args_z.as_ptr(), envp) };
                err = errno();
                match err {
                    libc::EACCES => seen_eacces = true,
                    libc::ENOENT | libc::ENOTDIR => {}
                    _ => break 'execve err,
                }
            }

            if seen_eacces {
                break 'execve libc::EACCES;
            }
            err
        };

        // If we are executing this code, the exec failed. We're in the
        // child process so there isn't much we can do. We try to output
        // something reasonable. Its important to note we MUST NOT return
        // any other error condition from here on out.
        if err == libc::ENOENT {
            write_stderr(
                b"Requested executable not found. Please verify the command is on\n\
                  the PATH and try again.\n",
            );
        } else {
            write_stderr(b"exec syscall failed with unexpected error: E");
            match errno_name(err) {
                Some(name) => write_stderr(name.as_bytes()),
                None => {
                    let mut buf = [0u8; 12];
                    write_stderr(format_i32(err, &mut buf));
                }
            }
            write_stderr(b"\n");
        }

        // We return a very specific error that can be detected to determine
        // we're in the child.
        Err(StartError::ExecFailedInChild)
    }

    #[cfg(windows)]
    fn start_windows(&mut self) -> Result<(), StartError> {
        use crate::os::windows::{self as windows, exp::kernel32, exp::ntdll};

        fn to_wide(s: &str) -> Vec<u16> {
            s.encode_utf16().chain(std::iter::once(0)).collect()
        }

        let cwd_w = self.cwd.as_ref().map(|cwd| to_wide(&cwd.to_string_lossy()));

        // Pass null for lpApplicationName and put the program as the first
        // token of lpCommandLine. This lets CreateProcessW perform the
        // standard program search (parent-app dir, CWD, system dirs, PATH)
        // and append ".exe" when the name has no extension, which is what
        // users expect for bare commands like `wsl ~` or `pwsh.exe`.
        // It also preserves the child's argv[0] as written by the caller
        // rather than replacing it with the resolved absolute path.
        let argv: Vec<String> = if !self.args.is_empty() {
            self.args
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        } else {
            vec![self.path.to_string_lossy().into_owned()]
        };
        let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
        let command_line = windows_create_command_line(&argv_refs);
        let mut command_line_w = to_wide(&command_line);
        let mut env_w = self.env.as_ref().map(create_windows_env_block);

        let any_null_fd = self.stdin.is_none() || self.stdout.is_none() || self.stderr.is_none();
        let null_fd: Option<windows::HANDLE> = if any_null_fd {
            // path = "\Device\Null"
            let mut path: Vec<u16> = "\\Device\\Null".encode_utf16().collect();
            let mut path_unicode_string = windows::UNICODE_STRING {
                Length: (path.len() * 2) as u16,
                MaximumLength: (path.len() * 2) as u16,
                Buffer: path.as_mut_ptr(),
            };
            let mut attrs =
                windows::OBJECT_ATTRIBUTES::new(std::ptr::null_mut(), &mut path_unicode_string);

            let mut fd: windows::HANDLE = std::ptr::null_mut();
            let mut io_status = windows::IO_STATUS_BLOCK::default(); // unused
                                                                     // SAFETY: valid out pointers and attributes.
            let result = unsafe {
                ntdll::NtCreateFile(
                    &mut fd,
                    windows::GENERIC_READ | windows::SYNCHRONIZE,
                    &mut attrs,
                    &mut io_status,
                    std::ptr::null_mut(),
                    windows::FILE_ATTRIBUTE_NORMAL,
                    windows::FILE_SHARE_READ,
                    windows::OPEN_EXISTING,
                    windows::FILE_NON_DIRECTORY_FILE,
                    std::ptr::null_mut(),
                    0,
                )
            };

            if result != windows::status::SUCCESS {
                let _ = windows::unexpected_status(result);
                return Err(StartError::Unexpected);
            }

            Some(fd)
        } else {
            None
        };
        struct CloseOnDrop(Option<windows::HANDLE>);
        impl Drop for CloseOnDrop {
            fn drop(&mut self) {
                if let Some(fd) = self.0 {
                    // SAFETY: closing a handle we own.
                    unsafe { kernel32::CloseHandle(fd) };
                }
            }
        }
        let _null_fd_guard = CloseOnDrop(null_fd);

        // TODO: In the case of having FDs instead of pty, need to set up
        // attributes such that the child process only inherits these handles,
        // then set bInheritsHandles below.

        let mut attribute_list_buf: Vec<u8> = Vec::new();
        let (attribute_list, stdin, stdout, stderr) =
            if let Some(pseudo_console) = self.pseudo_console {
                let mut attribute_list_size: usize = 0;
                // SAFETY: querying the required size.
                unsafe {
                    kernel32::InitializeProcThreadAttributeList(
                        std::ptr::null_mut(),
                        1,
                        0,
                        &mut attribute_list_size,
                    )
                };

                attribute_list_buf = vec![0u8; attribute_list_size];
                // SAFETY: the buffer has the required size.
                if unsafe {
                    kernel32::InitializeProcThreadAttributeList(
                        attribute_list_buf.as_mut_ptr().cast(),
                        1,
                        0,
                        &mut attribute_list_size,
                    )
                } == windows::FALSE
                {
                    let _ = windows::unexpected_error(windows::get_last_error());
                    return Err(StartError::Unexpected);
                }

                // SAFETY: valid attribute list and value.
                if unsafe {
                    kernel32::UpdateProcThreadAttribute(
                        attribute_list_buf.as_mut_ptr().cast(),
                        0,
                        windows::PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                        pseudo_console,
                        std::mem::size_of::<windows::HPCON>(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    )
                } == windows::FALSE
                {
                    let _ = windows::unexpected_error(windows::get_last_error());
                    return Err(StartError::Unexpected);
                }

                (
                    attribute_list_buf.as_mut_ptr() as windows::LPPROC_THREAD_ATTRIBUTE_LIST,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            } else {
                let stdin = self.stdin.unwrap_or_else(|| null_fd.unwrap());
                let stdout = self.stdout.unwrap_or_else(|| null_fd.unwrap());
                let stderr = self.stderr.unwrap_or_else(|| null_fd.unwrap());
                (std::ptr::null_mut(), stdin, stdout, stderr)
            };

        let mut startup_info_ex = windows::STARTUPINFOEX {
            StartupInfo: windows::STARTUPINFOW {
                cb: if !attribute_list.is_null() {
                    std::mem::size_of::<windows::STARTUPINFOEX>() as u32
                } else {
                    std::mem::size_of::<windows::STARTUPINFOW>() as u32
                },
                hStdError: stderr,
                hStdOutput: stdout,
                hStdInput: stdin,
                dwFlags: windows::STARTF_USESTDHANDLES,
                lpReserved: std::ptr::null_mut(),
                lpDesktop: std::ptr::null_mut(),
                lpTitle: std::ptr::null_mut(),
                dwX: 0,
                dwY: 0,
                dwXSize: 0,
                dwYSize: 0,
                dwXCountChars: 0,
                dwYCountChars: 0,
                dwFillAttribute: 0,
                wShowWindow: 0,
                cbReserved2: 0,
                lpReserved2: std::ptr::null_mut(),
            },
            lpAttributeList: attribute_list,
        };

        let mut flags: windows::DWORD = windows::CREATE_UNICODE_ENVIRONMENT;
        if !attribute_list.is_null() {
            flags |= windows::EXTENDED_STARTUPINFO_PRESENT;
        }

        let mut cwd_w = cwd_w;
        // SAFETY: zeroed PROCESS_INFORMATION is a valid out value.
        let mut process_information: windows::PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: all pointers are valid for the duration of the call.
        if unsafe {
            kernel32::CreateProcessW(
                std::ptr::null_mut(),
                command_line_w.as_mut_ptr(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                windows::TRUE,
                flags,
                env_w
                    .as_mut()
                    .map_or(std::ptr::null_mut(), |w| w.as_mut_ptr().cast()),
                cwd_w
                    .as_mut()
                    .map_or(std::ptr::null_mut(), |w| w.as_mut_ptr()),
                &mut startup_info_ex.StartupInfo,
                &mut process_information,
            )
        } == windows::FALSE
        {
            let _ = windows::unexpected_error(windows::get_last_error());
            return Err(StartError::Unexpected);
        }
        drop(attribute_list_buf);

        self.pid = Some(process_information.hProcess);
        Ok(())
    }

    /// Wait for the command to exit and return information about how it exited.
    pub fn wait(&self, block: bool) -> Result<Exit, WaitError> {
        #[cfg(windows)]
        {
            use crate::os::windows::{self as windows, exp::kernel32};
            let _ = block;
            // Block until the process exits. This returns immediately if the
            // process already exited.
            // SAFETY: the pid is a valid process handle.
            let result =
                unsafe { kernel32::WaitForSingleObject(self.pid.unwrap(), windows::INFINITE) };
            if result == windows::WAIT_FAILED {
                let _ = windows::unexpected_error(windows::get_last_error());
                return Err(WaitError::Unexpected);
            }

            let mut exit_code: windows::DWORD = 0;
            // SAFETY: valid out pointer.
            let has_code =
                unsafe { kernel32::GetExitCodeProcess(self.pid.unwrap(), &mut exit_code) }
                    != windows::FALSE;
            if !has_code {
                let _ = windows::unexpected_error(windows::get_last_error());
                return Err(WaitError::Unexpected);
            }

            return Ok(Exit::Exited(exit_code));
        }

        #[cfg(unix)]
        {
            let pid = self.pid.expect("command not started");
            let status: u32 = if block {
                let mut status: libc::c_int = 0;
                wait_pid(pid, &mut status, 0)?;
                status as u32
            } else {
                // We specify NOHANG because its not our fault if the process we launch
                // for the tty doesn't properly waitpid its children. We don't want
                // to hang the terminal over it.
                // When NOHANG is specified, waitpid will return a pid of 0 if the process
                // doesn't have a status to report. When that happens, it is as though the
                // wait call has not been performed, so we need to keep trying until we get
                // a non-zero pid back, otherwise we end up with zombie processes.
                loop {
                    let mut status: libc::c_int = 0;
                    let res = wait_pid(pid, &mut status, libc::WNOHANG)?;
                    if res != 0 {
                        break status as u32;
                    }
                }
            };

            Ok(Exit::init(status))
        }
    }

    /// Sets command->data to data.
    pub fn set_data(&mut self, pointer: *mut c_void) {
        self.data = pointer;
    }

    /// Returns command->data.
    pub fn get_data<DT>(&self) -> Option<*mut DT> {
        if self.data.is_null() {
            None
        } else {
            Some(self.data as *mut DT)
        }
    }

    /// If cmd.start fails with error.ExecFailedInChild it's the _child_
    /// process that is running. If it does not terminate in response to that
    /// error both the parent and child will continue as if they _are_ the
    /// test suite process.
    #[cfg(test)]
    fn testing_start(&mut self) -> Result<(), StartError> {
        match self.start() {
            Err(StartError::ExecFailedInChild) => {
                // I am a child process, I must not get confused and continue
                // running the rest of the test suite.
                // SAFETY: terminating the forked child.
                unsafe { libc::_exit(1) }
            }
            other => other,
        }
    }
}

/// Wrapper for the raw fork syscall. This preserves the error handling from
/// the std.posix wrapper that was removed in Zig 0.16.
#[cfg(unix)]
fn fork() -> Result<libc::pid_t, StartError> {
    // SAFETY: the child only performs async-signal-safe operations before
    // exec (see module docs).
    let rc = unsafe { libc::fork() };
    if rc >= 0 {
        return Ok(rc);
    }
    match errno() {
        libc::EAGAIN | libc::ENOMEM => Err(StartError::SystemResources),
        err => {
            log::warn!("unexpected fork errno={err}");
            Err(StartError::Unexpected)
        }
    }
}

/// Errors from [`setup_fd`] (Zig: the inferred error set of `PosixCall.f`).
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SetupFdError {
    Locked,
    FileBusy,
    PermissionDenied,
    ProcessFdQuotaExceeded,
    DeadLock,
    LockedRegionLimitExceeded,
    Unexpected,
}

/// Retry `f` on EINTR and map errno like the Zig `PosixCall` helper.
#[cfg(unix)]
fn posix_call(f: impl Fn() -> libc::c_int) -> Result<libc::c_int, SetupFdError> {
    loop {
        let rc = f();
        if rc >= 0 {
            return Ok(rc);
        }
        return Err(match errno() {
            libc::EINTR => continue,
            libc::EAGAIN | libc::EACCES => SetupFdError::Locked,
            libc::EBUSY => SetupFdError::FileBusy,
            libc::EPERM => SetupFdError::PermissionDenied,
            libc::EMFILE => SetupFdError::ProcessFdQuotaExceeded,
            libc::EDEADLK => SetupFdError::DeadLock,
            libc::ENOLCK => SetupFdError::LockedRegionLimitExceeded,
            // EBADF / EINVAL / ENOTDIR are programmer errors (unreachable in
            // Zig); in the forked child we can't panic, so report them.
            _ => SetupFdError::Unexpected,
        });
    }
}

#[cfg(unix)]
fn setup_fd(src: FileHandle, target: i32) -> Result<(), SetupFdError> {
    #[cfg(target_os = "linux")]
    {
        // We use dup3 so that we can clear CLO_ON_EXEC. We do NOT want this
        // file descriptor to be closed on exec since we're exactly exec-ing after
        // this.
        // SAFETY: plain syscall.
        posix_call(|| unsafe { libc::dup3(src, target, 0) })?;
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    {
        // Mac doesn't support dup3 so we use dup2. We purposely clear
        // CLO_ON_EXEC for this fd.
        // SAFETY: plain fcntl.
        let flags = posix_call(|| unsafe { libc::fcntl(src, libc::F_GETFD) })?;
        if flags & libc::FD_CLOEXEC != 0 {
            // SAFETY: plain fcntl.
            posix_call(|| unsafe { libc::fcntl(src, libc::F_SETFD, flags & !libc::FD_CLOEXEC) })?;
        }

        // SAFETY: plain syscall.
        posix_call(|| unsafe { libc::dup2(src, target) })?;
        Ok(())
    }
}

/// Wrapper for the raw waitpid syscall. Status is only initialized on success;
/// interrupted waits are retried and all other errors are propagated.
#[cfg(unix)]
fn wait_pid(
    pid: libc::pid_t,
    status: &mut libc::c_int,
    flags: libc::c_int,
) -> Result<libc::pid_t, WaitError> {
    loop {
        // SAFETY: valid out pointer.
        let rc = unsafe { libc::waitpid(pid, status, flags) };
        if rc >= 0 {
            return Ok(rc);
        }
        return Err(match errno() {
            libc::EINTR => continue,
            libc::ECHILD => WaitError::NoChildProcess,
            libc::EINVAL => WaitError::InvalidWaitOptions,
            err => {
                log::warn!("unexpected waitpid errno={err}");
                WaitError::Unexpected
            }
        });
    }
}

/// Write to stderr with a raw syscall (async-signal-safe).
#[cfg(unix)]
fn write_stderr(mut bytes: &[u8]) {
    while !bytes.is_empty() {
        // SAFETY: valid buffer.
        let n = unsafe { libc::write(libc::STDERR_FILENO, bytes.as_ptr().cast(), bytes.len()) };
        if n < 0 {
            if errno() == libc::EINTR {
                continue;
            }
            return;
        }
        bytes = &bytes[n as usize..];
    }
}

/// Format an integer into a stack buffer without allocating.
#[cfg(unix)]
fn format_i32(v: i32, buf: &mut [u8; 12]) -> &[u8] {
    let neg = v < 0;
    let mut n = v.unsigned_abs();
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    if neg {
        i -= 1;
        buf[i] = b'-';
    }
    &buf[i..]
}

/// The Zig `@tagName` of common `posix.E` values (without the `E` prefix).
#[cfg(unix)]
fn errno_name(err: i32) -> Option<&'static str> {
    Some(match err {
        libc::EPERM => "PERM",
        libc::ENOENT => "NOENT",
        libc::EIO => "IO",
        libc::E2BIG => "2BIG",
        libc::ENOEXEC => "NOEXEC",
        libc::EBADF => "BADF",
        libc::ENOMEM => "NOMEM",
        libc::EACCES => "ACCES",
        libc::EFAULT => "FAULT",
        libc::ENOTDIR => "NOTDIR",
        libc::EISDIR => "ISDIR",
        libc::EINVAL => "INVAL",
        libc::ENFILE => "NFILE",
        libc::EMFILE => "MFILE",
        libc::ETXTBSY => "TXTBSY",
        libc::ENAMETOOLONG => "NAMETOOLONG",
        libc::ELOOP => "LOOP",
        _ => return None,
    })
}

// Copied from Zig. This is a publicly exported function but there is no
// way to get it from the std package.
fn create_null_delimited_env_map(env_map: &EnvMap) -> Result<Vec<CString>, StartError> {
    let envp_count = env_map.len();
    let mut envp_buf: Vec<CString> = Vec::with_capacity(envp_count);

    for (key, value) in env_map {
        let mut env_buf = Vec::with_capacity(key.len() + value.len() + 1);
        env_buf.extend_from_slice(key.as_bytes());
        env_buf.push(b'=');
        env_buf.extend_from_slice(value.as_bytes());
        envp_buf.push(CString::new(env_buf).map_err(|_| StartError::InvalidArgument)?);
    }
    debug_assert_eq!(envp_buf.len(), envp_count);

    Ok(envp_buf)
}

// Copied from Zig. This is a publicly exported function but there is no
// way to get it from the std package.
#[cfg_attr(not(windows), allow(dead_code))]
fn create_windows_env_block(env_map: &EnvMap) -> Vec<u16> {
    let mut result: Vec<u16> = Vec::new();
    for (key, value) in env_map {
        result.extend(key.encode_utf16());
        result.push(b'=' as u16);
        result.extend(value.encode_utf16());
        result.push(0);
    }
    // 4 for the final 4 null bytes
    result.extend_from_slice(&[0, 0, 0, 0]);
    result
}

/// Copied from Zig. This function could be made public in child_process.zig instead.
#[cfg_attr(not(windows), allow(dead_code))]
fn windows_create_command_line(argv: &[&str]) -> String {
    let mut buf = String::new();

    for (arg_i, arg) in argv.iter().enumerate() {
        if arg_i != 0 {
            buf.push(' ');
        }
        if !arg.contains([' ', '\t', '\n', '"']) {
            buf.push_str(arg);
            continue;
        }
        buf.push('"');
        let mut backslash_count: usize = 0;
        for ch in arg.chars() {
            match ch {
                '\\' => backslash_count += 1,
                '"' => {
                    buf.extend(std::iter::repeat_n('\\', backslash_count * 2 + 1));
                    buf.push('"');
                    backslash_count = 0;
                }
                _ => {
                    buf.extend(std::iter::repeat_n('\\', backslash_count));
                    buf.push(ch);
                    backslash_count = 0;
                }
            }
        }
        buf.extend(std::iter::repeat_n('\\', backslash_count * 2));
        buf.push('"');
    }

    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cstrs(v: &[&str]) -> Vec<CString> {
        v.iter().map(|s| CString::new(*s).unwrap()).collect()
    }

    #[test]
    fn create_null_delimited_env_map_test() {
        let mut envmap = EnvMap::new();
        envmap.insert("HOME".into(), "/home/ifreund".into());
        envmap.insert("WAYLAND_DISPLAY".into(), "wayland-1".into());
        envmap.insert("DISPLAY".into(), ":1".into());
        envmap.insert("DEBUGINFOD_URLS".into(), " ".into());
        envmap.insert("XCURSOR_SIZE".into(), "24".into());

        let environ = create_null_delimited_env_map(&envmap).unwrap();

        assert_eq!(environ.len(), 5);

        for target in [
            "HOME=/home/ifreund",
            "WAYLAND_DISPLAY=wayland-1",
            "DISPLAY=:1",
            "DEBUGINFOD_URLS= ",
            "XCURSOR_SIZE=24",
        ] {
            assert!(
                environ.iter().any(|v| v.as_bytes() == target.as_bytes()),
                "Environment variable not found: {target}"
            );
        }
    }

    #[test]
    fn windows_command_line_quoting() {
        assert_eq!(windows_create_command_line(&["wsl", "~"]), "wsl ~");
        assert_eq!(
            windows_create_command_line(&["a b", "c\"d", "e\\\"f", "g\\ h\\"]),
            "\"a b\" \"c\\\"d\" \"e\\\\\\\"f\" \"g\\ h\\\\\""
        );
    }

    #[test]
    fn windows_env_block() {
        let mut env = EnvMap::new();
        env.insert("A".into(), "b".into());
        let block = create_windows_env_block(&env);
        let expected: Vec<u16> = "A=b\0\0\0\0\0".encode_utf16().collect();
        assert_eq!(block, expected);
    }

    #[cfg(unix)]
    fn sh_command(args: &[&str]) -> Command {
        Command {
            path: CString::new("/bin/sh").unwrap(),
            args: cstrs(args),
            ..Default::default()
        }
    }

    #[cfg(unix)]
    #[test]
    fn command_os_pre_exec_1() {
        let mut cmd = Command {
            os_pre_exec: Some(|_| {
                // This runs in the child, so we can exit and it won't
                // kill the test runner.
                // SAFETY: terminating the forked child.
                unsafe { libc::_exit(42) }
            }),
            ..sh_command(&["/bin/sh", "-v"])
        };

        cmd.testing_start().unwrap();
        assert!(cmd.pid.is_some());
        let exit = cmd.wait(true).unwrap();
        assert_eq!(exit, Exit::Exited(42));
    }

    #[cfg(unix)]
    #[test]
    fn command_os_pre_exec_2() {
        let mut cmd = Command {
            os_pre_exec: Some(|_| {
                // This runs in the child, so we can exit and it won't
                // kill the test runner.
                Some(42)
            }),
            ..sh_command(&["/bin/sh", "-v"])
        };

        cmd.testing_start().unwrap();
        assert!(cmd.pid.is_some());
        let exit = cmd.wait(true).unwrap();
        assert_eq!(exit, Exit::Exited(42));
    }

    #[cfg(unix)]
    #[test]
    fn command_rt_pre_exec_1() {
        let mut cmd = Command {
            rt_pre_exec: Some(|_| {
                // This runs in the child, so we can exit and it won't
                // kill the test runner.
                // SAFETY: terminating the forked child.
                unsafe { libc::_exit(42) }
            }),
            ..sh_command(&["/bin/sh", "-v"])
        };

        cmd.testing_start().unwrap();
        assert!(cmd.pid.is_some());
        let exit = cmd.wait(true).unwrap();
        assert_eq!(exit, Exit::Exited(42));
    }

    #[cfg(unix)]
    #[test]
    fn command_rt_pre_exec_2() {
        let mut cmd = Command {
            rt_pre_exec: Some(|_| {
                // This runs in the child, so we can exit and it won't
                // kill the test runner.
                Some(42)
            }),
            ..sh_command(&["/bin/sh", "-v"])
        };

        cmd.testing_start().unwrap();
        assert!(cmd.pid.is_some());
        let exit = cmd.wait(true).unwrap();
        assert_eq!(exit, Exit::Exited(42));
    }

    #[cfg(unix)]
    #[test]
    fn command_rt_post_fork_1() {
        let mut cmd = Command {
            rt_post_fork: Some(|_| Err(PostForkError::PostForkError)),
            ..sh_command(&["/bin/sh", "-c", "sleep 1"])
        };

        assert_eq!(cmd.testing_start(), Err(StartError::PostForkError));
        // Reap the child (the Zig test leaks it).
        let _ = cmd.wait(true);
    }

    /// A temp dir with stdout.txt / stderr.txt (Zig: `TempDir` +
    /// `createTestStdout` / `createTestStderr`).
    #[cfg(unix)]
    struct TestFiles {
        td: crate::os::temp_dir::TempDir,
    }

    #[cfg(unix)]
    impl TestFiles {
        fn new() -> Self {
            Self {
                td: crate::os::temp_dir::TempDir::init().unwrap(),
            }
        }

        fn create(&self, name: &str) -> std::fs::File {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(self.td.path().join(name))
                .unwrap()
        }

        fn read(file: &std::fs::File) -> Vec<u8> {
            use std::os::unix::fs::FileExt;
            let size = file.metadata().unwrap().len() as usize;
            let mut data = vec![0u8; size];
            assert_eq!(file.read_at(&mut data, 0).unwrap(), size);
            data
        }
    }

    #[cfg(unix)]
    #[test]
    fn command_redirect_stdout_to_file() {
        use std::os::fd::AsRawFd;
        let td = TestFiles::new();
        let stdout = td.create("stdout.txt");

        let mut cmd = Command {
            stdout: Some(stdout.as_raw_fd()),
            ..sh_command(&["/bin/sh", "-c", "echo hello"])
        };

        cmd.testing_start().unwrap();
        assert!(cmd.pid.is_some());
        let exit = cmd.wait(true).unwrap();
        assert_eq!(exit, Exit::Exited(0));

        // Read our stdout
        let contents = TestFiles::read(&stdout);
        assert!(!contents.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn command_custom_env_vars() {
        use std::os::fd::AsRawFd;
        let td = TestFiles::new();
        let stdout = td.create("stdout.txt");

        let mut env = EnvMap::new();
        env.insert("VALUE".into(), "hello".into());

        let mut cmd = Command {
            stdout: Some(stdout.as_raw_fd()),
            env: Some(env),
            ..sh_command(&["/bin/sh", "-c", "echo $VALUE"])
        };

        cmd.testing_start().unwrap();
        assert!(cmd.pid.is_some());
        let exit = cmd.wait(true).unwrap();
        assert_eq!(exit, Exit::Exited(0));

        // Read our stdout
        let contents = TestFiles::read(&stdout);
        assert_eq!(contents, b"hello\n");
    }

    #[cfg(unix)]
    #[test]
    fn command_custom_working_directory() {
        use std::os::fd::AsRawFd;
        let td = TestFiles::new();
        let stdout = td.create("stdout.txt");

        let mut cmd = Command {
            stdout: Some(stdout.as_raw_fd()),
            cwd: Some(CString::new("/tmp").unwrap()),
            ..sh_command(&["/bin/sh", "-c", "pwd"])
        };

        cmd.testing_start().unwrap();
        assert!(cmd.pid.is_some());
        let exit = cmd.wait(true).unwrap();
        assert_eq!(exit, Exit::Exited(0));

        // Read our stdout
        let contents = TestFiles::read(&stdout);
        if cfg!(target_os = "macos") {
            assert_eq!(contents, b"/private/tmp\n");
        } else {
            assert_eq!(contents, b"/tmp\n");
        }
    }

    // Test validate an execveZ failure correctly terminates when error.ExecFailedInChild is correctly handled
    //
    // Incorrectly handling an error.ExecFailedInChild results in a second copy of the test process running.
    // Duplicating the test process leads to weird behavior
    // zig build test will hang
    // test binary created via -Demit-test-exe will run 2 copies of the test suite
    #[cfg(unix)]
    #[test]
    fn command_posix_fork_handles_execve_failure() {
        use std::os::fd::AsRawFd;
        let td = TestFiles::new();
        let stdout = td.create("stdout.txt");
        let stderr = td.create("stderr.txt");

        let mut cmd = Command {
            path: CString::new("/not/a/binary").unwrap(),
            args: cstrs(&["/not/a/binary", ""]),
            stdout: Some(stdout.as_raw_fd()),
            stderr: Some(stderr.as_raw_fd()),
            cwd: Some(CString::new("/bin").unwrap()),
            ..Default::default()
        };

        cmd.testing_start().unwrap();
        assert!(cmd.pid.is_some());
        let exit = cmd.wait(true).unwrap();
        assert_eq!(exit, Exit::Exited(1));

        // (Rust port addition) the child explained the failure on stderr.
        let err = TestFiles::read(&stderr);
        assert!(err.starts_with(b"Requested executable not found."));
    }

    #[cfg(unix)]
    #[test]
    fn command_path_search() {
        // (Rust port addition) a bare command name is resolved via PATH.
        use std::os::fd::AsRawFd;
        let td = TestFiles::new();
        let stdout = td.create("stdout.txt");
        let mut cmd = Command {
            path: CString::new("sh").unwrap(),
            args: cstrs(&["sh", "-c", "echo found"]),
            stdout: Some(stdout.as_raw_fd()),
            ..Default::default()
        };
        cmd.testing_start().unwrap();
        assert_eq!(cmd.wait(false).unwrap(), Exit::Exited(0));
        assert_eq!(TestFiles::read(&stdout), b"found\n");
    }

    #[cfg(unix)]
    #[test]
    fn exit_init() {
        assert_eq!(Exit::init(42 << 8), Exit::Exited(42));
        assert_eq!(
            Exit::init(libc::SIGKILL as u32),
            Exit::Signal(libc::SIGKILL as u32)
        );
        assert_eq!(
            Exit::init(0x7f | (libc::SIGSTOP as u32) << 8),
            Exit::Stopped(libc::SIGSTOP as u32)
        );
    }

    #[cfg(unix)]
    #[test]
    fn data_roundtrip() {
        let mut cmd = Command::default();
        assert!(cmd.get_data::<u32>().is_none());
        let mut v: u32 = 7;
        cmd.set_data(&mut v as *mut u32 as *mut c_void);
        assert_eq!(unsafe { *cmd.get_data::<u32>().unwrap() }, 7);
    }

    #[cfg(unix)]
    #[test]
    fn format_i32_works() {
        let mut buf = [0u8; 12];
        assert_eq!(format_i32(-123, &mut buf), b"-123");
        assert_eq!(format_i32(0, &mut buf), b"0");
        assert_eq!(errno_name(libc::EACCES), Some("ACCES"));
    }
}
