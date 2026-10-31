//! Port of `src/os/flatpak.zig`.
//!
//! STATUS: ported
//!
//! `FlatpakHostCommand` requires GIO/GLib and is only compiled with the
//! `gtk` Cargo feature on Linux; `is_flatpak` is always available.

/// Returns true if we're running in a Flatpak environment.
pub fn is_flatpak() -> bool {
    // If we're not on Linux then we'll make this always false.
    if !cfg!(target_os = "linux") {
        return false;
    }
    #[cfg(unix)]
    {
        // SAFETY: valid NUL-terminated path.
        return unsafe { libc::access(c"/.flatpak-info".as_ptr(), libc::F_OK) } == 0;
    }
    #[allow(unreachable_code)]
    false
}

#[cfg(all(target_os = "linux", feature = "gtk"))]
pub use self::host_command::*;

#[cfg(all(target_os = "linux", feature = "gtk"))]
mod host_command {
    use std::ffi::{c_char, c_int, c_uint, c_void, CStr, CString};
    use std::fmt;
    use std::os::fd::RawFd;
    use std::sync::{Arc, Condvar, Mutex, MutexGuard};

    use gio::ffi as gio_c;
    use glib::ffi as glib_c;
    use glib::gobject_ffi as gobject_c;

    use crate::os::EnvironMap;

    /// The event loop used to deliver asynchronous wait results.
    ///
    /// NOTE(port): Zig arms a zero-duration `xev.Timer` on the caller's
    /// `xev.Loop` so the callback runs on the loop's thread on its next
    /// tick, but `onExit` arms it from the GLib thread, which is a data race
    /// on the single-threaded loop (UB in Rust). Implementors of this trait
    /// must hand `f` to the loop thread in a thread-safe way (e.g. a queue
    /// plus `xev.Async`). Flatpak host commands are not built into the
    /// Ghostty application (`build_config::FLATPAK`), so nothing calls this.
    pub trait XevLoop: Send + Sync {
        fn run_soon(&self, f: Box<dyn FnOnce() + Send>);
    }

    /// Flags for HostCommand method
    ///
    /// Ref: https://docs.flatpak.org/en/latest/libflatpak-api-reference.html#gdbus-method-org-freedesktop-Flatpak-Development.HostCommand
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    struct Flags {
        /// Clear the environment
        clear_env: bool,
        /// Kill the sandbox when the caller disappears from the session bus
        watch_bus: bool,
    }

    impl Flags {
        /// Zig: `@bitCast(flags)` of `packed struct(c_uint)`.
        fn to_bits(self) -> c_uint {
            (self.clear_env as c_uint) | ((self.watch_bus as c_uint) << 1)
        }
    }

    /// Errors that are possible from us.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Error {
        FlatpakMustBeStarted,
        FlatpakSpawnFail,
        FlatpakSetupFail,
        FlatpakRPCFail,
    }

    impl fmt::Display for Error {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            fmt::Debug::fmt(self, f)
        }
    }

    impl std::error::Error for Error {}

    /// Zig: `xev.Timer.RunError || Error`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum WaitError {
        Flatpak(Error),
        Timer,
    }

    impl fmt::Display for WaitError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            fmt::Debug::fmt(self, f)
        }
    }

    impl std::error::Error for WaitError {}

    pub type WaitCallback = Box<dyn FnOnce(Result<u8, WaitError>) + Send>;

    /// An asynchronous wait request (Zig: `Completion`).
    pub struct Completion {
        pub callback: WaitCallback,
    }

    /// Raw GLib pointer that may cross threads (GMainLoop is thread-safe).
    #[derive(Clone, Copy)]
    struct LoopPtr(*mut glib_c::GMainLoop);
    // SAFETY: GMainLoop is reference counted and thread-safe for quit.
    unsafe impl Send for LoopPtr {}

    /// State the process is in. This can't be inspected directly, you
    /// must use getters on the struct to get access.
    enum State {
        /// Initial state
        Init,

        /// Error starting. The error message is only available via logs.
        /// (This isn't a fundamental limitation, just didn't need the
        /// error message yet)
        Err,

        /// Process started with the given pid on the host.
        Started {
            pid: u32,
            loop_xev: Option<Arc<dyn XevLoop>>,
            completion: Option<Completion>,
            subscription: c_uint,
            loop_: LoopPtr,
        },

        /// Process exited
        Exited { pid: u32, status: u8 },
    }

    struct Shared {
        /// State of the process. This is updated by the dedicated thread it
        /// runs in and is protected by the given lock and condition
        /// variable.
        state: Mutex<State>,
        state_cv: Condvar,
    }

    impl Shared {
        fn lock(&self) -> MutexGuard<'_, State> {
            self.state.lock().unwrap_or_else(|e| e.into_inner())
        }

        /// Helper to update the state and notify waiters via the cv.
        fn update_state(&self, state: State) {
            let mut guard = self.lock();
            *guard = state;
            self.state_cv.notify_all();
        }
    }

    /// The parameters the command thread needs.
    struct Params {
        argv: Vec<String>,
        cwd: Option<CString>,
        env: Option<EnvironMap>,
        stdin: RawFd,
        stdout: RawFd,
        stderr: RawFd,
    }

    /// A struct to help execute commands on the host via the
    /// org.freedesktop.Flatpak.Development DBus module. This uses GIO/GLib
    /// under the hood.
    ///
    /// This always spawns its own thread and maintains its own GLib event
    /// loop. This makes it easy for the command to behave synchronously
    /// similar to std::process::Child.
    ///
    /// There are lots of chances for low-hanging improvements here
    /// (automatic pipes, /dev/null, etc.) but this was purpose built for my
    /// needs so it doesn't have all of those.
    ///
    /// Requires GIO, GLib to be available and linked. The Zig version
    /// requires a stable `self` pointer; the Rust port keeps the shared
    /// state in an `Arc` instead.
    pub struct FlatpakHostCommand {
        /// Argv are the arguments to call on the host with argv[0] being
        /// the command to execute.
        pub argv: Vec<String>,

        /// The cwd for the new process. If this is not set then it will use
        /// the current cwd of the calling process.
        pub cwd: Option<CString>,

        /// Environment variables for the child process. If this is None,
        /// this does not send any environment variables.
        pub env: Option<EnvironMap>,

        /// File descriptors to send to the child process. It is up to the
        /// caller to create the file descriptors and set them up.
        pub stdin: RawFd,
        pub stdout: RawFd,
        pub stderr: RawFd,

        shared: Arc<Shared>,
    }

    impl FlatpakHostCommand {
        pub fn new(
            argv: Vec<String>,
            stdin: RawFd,
            stdout: RawFd,
            stderr: RawFd,
        ) -> FlatpakHostCommand {
            FlatpakHostCommand {
                argv,
                cwd: None,
                env: None,
                stdin,
                stdout,
                stderr,
                shared: Arc::new(Shared {
                    state: Mutex::new(State::Init),
                    state_cv: Condvar::new(),
                }),
            }
        }

        /// Spawn the command. This will start the host command. On return,
        /// the pid will be available. This must only be called with the
        /// state in "init".
        pub fn spawn(&self) -> Result<u32, Box<dyn std::error::Error + Send + Sync>> {
            let params = Params {
                argv: self.argv.clone(),
                cwd: self.cwd.clone(),
                env: self.env.clone(),
                stdin: self.stdin,
                stdout: self.stdout,
                stderr: self.stderr,
            };
            let shared = self.shared.clone();

            // We don't track this thread, it will terminate on its own on
            // command exit.
            std::thread::Builder::new()
                .name("flatpak-host-command".into())
                .spawn(move || thread_main(shared, params))?;

            // Wait for the process to start or error.
            let mut guard = self.shared.lock();
            while matches!(*guard, State::Init) {
                guard = self
                    .shared
                    .state_cv
                    .wait(guard)
                    .unwrap_or_else(|e| e.into_inner());
            }

            match &*guard {
                State::Init => unreachable!(),
                State::Err => Err(Box::new(Error::FlatpakSpawnFail)),
                State::Started { pid, .. } => Ok(*pid),
                State::Exited { pid, .. } => Ok(*pid),
            }
        }

        /// Wait for the process to end and return the exit status. This
        /// can only be called ONCE. Once this returns, the state is reset.
        pub fn wait(&self) -> Result<u8, Error> {
            let mut guard = self.shared.lock();

            loop {
                match &*guard {
                    State::Init => return Err(Error::FlatpakMustBeStarted),
                    State::Err => return Err(Error::FlatpakSpawnFail),
                    State::Started { .. } => {}
                    State::Exited { status, .. } => {
                        let status = *status;
                        *guard = State::Init;
                        self.shared.state_cv.notify_all();
                        return Ok(status);
                    }
                }

                guard = self
                    .shared
                    .state_cv
                    .wait(guard)
                    .unwrap_or_else(|e| e.into_inner());
            }
        }

        /// Wait for the process to end asynchronously via the event loop.
        /// This can only be called ONCE. The callback runs on `loop_`.
        pub fn wait_xev(&self, loop_: Arc<dyn XevLoop>, completion: Completion) {
            let mut guard = self.shared.lock();

            let result = match &mut *guard {
                State::Init => Err(WaitError::Flatpak(Error::FlatpakMustBeStarted)),
                State::Err => Err(WaitError::Flatpak(Error::FlatpakSpawnFail)),
                State::Started {
                    loop_xev,
                    completion: c,
                    ..
                } => {
                    *loop_xev = Some(loop_);
                    *c = Some(completion);
                    return;
                }
                State::Exited { status, .. } => Ok(*status),
            };
            drop(guard);

            let callback = completion.callback;
            loop_.run_soon(Box::new(move || callback(result)));
        }

        /// Send a signal to the started command. This does nothing if the
        /// command is not in the started state.
        pub fn signal(&self, sig: u8, pg: bool) -> Result<(), Error> {
            let pid = {
                let guard = self.shared.lock();
                match &*guard {
                    State::Started { pid, .. } => *pid,
                    _ => return Ok(()),
                }
            };

            // SAFETY: GIO calls with valid arguments; every owned object is
            // released before returning.
            unsafe {
                // Get our bus connection.
                let mut g_err: *mut glib_c::GError = std::ptr::null_mut();
                let bus = gio_c::g_bus_get_sync(
                    gio_c::G_BUS_TYPE_SESSION,
                    std::ptr::null_mut(),
                    &mut g_err,
                );
                if bus.is_null() {
                    log::warn!(target: "flatpak", "signal error getting bus: {}", gerror_message(g_err));
                    free_gerror(g_err);
                    return Err(Error::FlatpakSetupFail);
                }

                let reply = gio_c::g_dbus_connection_call_sync(
                    bus,
                    c"org.freedesktop.Flatpak".as_ptr(),
                    c"/org/freedesktop/Flatpak/Development".as_ptr(),
                    c"org.freedesktop.Flatpak.Development".as_ptr(),
                    c"HostCommandSignal".as_ptr(),
                    glib_c::g_variant_new(
                        c"(uub)".as_ptr(),
                        pid as c_uint,
                        sig as c_uint,
                        pg as c_int,
                    ),
                    variant_type(c"()"),
                    gio_c::G_DBUS_CALL_FLAGS_NONE,
                    c_int::MAX,
                    std::ptr::null_mut(),
                    &mut g_err,
                );
                if !g_err.is_null() {
                    log::warn!(target: "flatpak", "signal send error: {}", gerror_message(g_err));
                    free_gerror(g_err);
                    gobject_c::g_object_unref(bus.cast());
                    return Ok(());
                }
                if !reply.is_null() {
                    glib_c::g_variant_unref(reply);
                }
                gobject_c::g_object_unref(bus.cast());
            }
            Ok(())
        }
    }

    /// Zig: `G_VARIANT_TYPE(s)` (a cast of the type string).
    fn variant_type(s: &'static CStr) -> *const glib_c::GVariantType {
        s.as_ptr().cast()
    }

    unsafe fn gerror_message(err: *mut glib_c::GError) -> String {
        if err.is_null() || (*err).message.is_null() {
            return String::new();
        }
        CStr::from_ptr((*err).message)
            .to_string_lossy()
            .into_owned()
    }

    unsafe fn free_gerror(err: *mut glib_c::GError) {
        if !err.is_null() {
            glib_c::g_error_free(err);
        }
    }

    fn thread_main(shared: Arc<Shared>, params: Params) {
        // SAFETY: GLib calls with valid arguments; every owned object is
        // released before returning.
        unsafe {
            // Create a new thread-local context so that all our sources go
            // to this context and we can run our loop correctly.
            let ctx = glib_c::g_main_context_new();
            glib_c::g_main_context_push_thread_default(ctx);

            // Get our loop for the current thread
            let loop_ = glib_c::g_main_loop_new(ctx, glib_c::GTRUE);

            // Get our bus connection. This has to remain active until we
            // exit the thread otherwise our signals won't be called.
            let mut g_err: *mut glib_c::GError = std::ptr::null_mut();
            let bus =
                gio_c::g_bus_get_sync(gio_c::G_BUS_TYPE_SESSION, std::ptr::null_mut(), &mut g_err);
            if bus.is_null() {
                log::warn!(target: "flatpak", "spawn error getting bus: {}", gerror_message(g_err));
                free_gerror(g_err);
                shared.update_state(State::Err);
            } else {
                // Spawn the command first. This will setup all our IO.
                match start(&shared, &params, bus, loop_) {
                    Ok(()) => {
                        // Run the event loop. It quits in the exit callback.
                        glib_c::g_main_loop_run(loop_);
                    }
                    Err(err) => {
                        log::warn!(target: "flatpak", "error starting host command: {err}");
                        shared.update_state(State::Err);
                    }
                }
                gobject_c::g_object_unref(bus.cast());
            }

            glib_c::g_main_loop_unref(loop_);
            glib_c::g_main_context_pop_thread_default(ctx);
            glib_c::g_main_context_unref(ctx);
        }
    }

    /// Start the command. This will start the host command and set the
    /// pid field on success. This will not wait for completion.
    unsafe fn start(
        shared: &Arc<Shared>,
        params: &Params,
        bus: *mut gio_c::GDBusConnection,
        loop_: *mut glib_c::GMainLoop,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut err: *mut glib_c::GError = std::ptr::null_mut();

        // Our list of file descriptors that we need to send to the process.
        let fd_list = gio_c::g_unix_fd_list_new();
        struct Unref(*mut gobject_c::GObject);
        impl Drop for Unref {
            fn drop(&mut self) {
                // SAFETY: we own one reference.
                unsafe { gobject_c::g_object_unref(self.0) };
            }
        }
        let _fd_list_guard = Unref(fd_list.cast());
        for fd in [params.stdin, params.stdout, params.stderr] {
            if gio_c::g_unix_fd_list_append(fd_list, fd, &mut err) < 0 {
                log::warn!(target: "flatpak", "error adding fd: {}", gerror_message(err));
                free_gerror(err);
                return Err(Box::new(Error::FlatpakSetupFail));
            }
        }

        // Build our arguments for the file descriptors.
        let fd_builder = glib_c::g_variant_builder_new(variant_type(c"a{uh}"));
        glib_c::g_variant_builder_add(fd_builder, c"{uh}".as_ptr(), 0 as c_int, params.stdin);
        glib_c::g_variant_builder_add(fd_builder, c"{uh}".as_ptr(), 1 as c_int, params.stdout);
        glib_c::g_variant_builder_add(fd_builder, c"{uh}".as_ptr(), 2 as c_int, params.stderr);

        // Build our env vars
        let env_builder = glib_c::g_variant_builder_new(variant_type(c"a{ss}"));
        if let Some(env) = &params.env {
            for (k, v) in env {
                let (Ok(key), Ok(value)) = (CString::new(k.as_str()), CString::new(v.as_str()))
                else {
                    continue;
                };
                glib_c::g_variant_builder_add(
                    env_builder,
                    c"{ss}".as_ptr(),
                    key.as_ptr(),
                    value.as_ptr(),
                );
            }
        }

        // Build our args
        let args_owned: Vec<CString> = params
            .argv
            .iter()
            .map(|a| CString::new(a.as_str()))
            .collect::<Result<_, _>>()?;
        let mut args: Vec<*const c_char> = args_owned.iter().map(|a| a.as_ptr()).collect();
        args.push(std::ptr::null());

        // Get the cwd in case we don't have ours set. A small optimization
        // would be to do this only if we need it but this isn't a
        // common code path.
        let g_cwd = glib_c::g_get_current_dir();

        // Terminate session if Ghostty drops off the bus (e.g. due to
        // crashes)
        let flags = Flags {
            watch_bus: true,
            ..Default::default()
        };

        // The params for our RPC call
        let cwd_ptr: *const c_char = match &params.cwd {
            Some(cwd) => cwd.as_ptr(),
            None => g_cwd,
        };
        let rpc_params = glib_c::g_variant_new(
            c"(^ay^aay@a{uh}@a{ss}u)".as_ptr(),
            cwd_ptr,
            args.as_ptr(),
            glib_c::g_variant_builder_end(fd_builder),
            glib_c::g_variant_builder_end(env_builder),
            flags.to_bits(),
        );
        glib_c::g_variant_ref_sink(rpc_params); // take ownership
        glib_c::g_variant_builder_unref(fd_builder);
        glib_c::g_variant_builder_unref(env_builder);
        glib_c::g_free(g_cwd.cast());

        // Subscribe to exit notifications
        let user_data = Arc::into_raw(shared.clone()) as *mut c_void;
        let subscription_id = gio_c::g_dbus_connection_signal_subscribe(
            bus,
            c"org.freedesktop.Flatpak".as_ptr(),
            c"org.freedesktop.Flatpak.Development".as_ptr(),
            c"HostCommandExited".as_ptr(),
            c"/org/freedesktop/Flatpak/Development".as_ptr(),
            std::ptr::null(),
            0,
            Some(on_exit),
            user_data,
            Some(drop_shared),
        );

        // Go!
        let reply = gio_c::g_dbus_connection_call_with_unix_fd_list_sync(
            bus,
            c"org.freedesktop.Flatpak".as_ptr(),
            c"/org/freedesktop/Flatpak/Development".as_ptr(),
            c"org.freedesktop.Flatpak.Development".as_ptr(),
            c"HostCommand".as_ptr(),
            rpc_params,
            variant_type(c"(u)"),
            gio_c::G_DBUS_CALL_FLAGS_NONE,
            c_int::MAX,
            fd_list,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut err,
        );
        glib_c::g_variant_unref(rpc_params);
        if reply.is_null() {
            log::warn!(target: "flatpak", "Flatpak.HostCommand failed: {}", gerror_message(err));
            free_gerror(err);
            gio_c::g_dbus_connection_signal_unsubscribe(bus, subscription_id);
            return Err(Box::new(Error::FlatpakRPCFail));
        }

        let mut pid: u32 = 0;
        glib_c::g_variant_get(reply, c"(u)".as_ptr(), &mut pid as *mut u32);
        glib_c::g_variant_unref(reply);
        log::debug!(target: "flatpak", "HostCommand started pid={pid} subscription={subscription_id}");

        shared.update_state(State::Started {
            pid,
            subscription: subscription_id,
            loop_: LoopPtr(loop_),
            completion: None,
            loop_xev: None,
        });
        Ok(())
    }

    unsafe extern "C" fn drop_shared(ud: *mut c_void) {
        // SAFETY: created with Arc::into_raw in `start`.
        drop(Arc::from_raw(ud as *const Shared));
    }

    unsafe extern "C" fn on_exit(
        bus: *mut gio_c::GDBusConnection,
        _sender: *const c_char,
        _object_path: *const c_char,
        _interface: *const c_char,
        _signal: *const c_char,
        params: *mut glib_c::GVariant,
        ud: *mut c_void,
    ) {
        // SAFETY: `ud` is the Arc<Shared> registered in `start`; it lives
        // until the subscription is removed.
        let shared = &*(ud as *const Shared);

        let mut pid: u32 = 0;
        let mut exit_status_raw: u32 = 0;
        glib_c::g_variant_get(
            params,
            c"(uu)".as_ptr(),
            &mut pid as *mut u32,
            &mut exit_status_raw as *mut u32,
        );

        let (exit_status, completion, loop_xev, subscription, loop_) = {
            let mut guard = shared.lock();
            let State::Started {
                pid: state_pid,
                loop_xev,
                completion,
                subscription,
                loop_,
            } = &mut *guard
            else {
                return;
            };
            if *state_pid != pid {
                return;
            }

            // Zig: `posix.W.EXITSTATUS`.
            let exit_status = ((exit_status_raw >> 8) & 0xff) as u8;
            let taken = (
                exit_status,
                completion.take(),
                loop_xev.take(),
                *subscription,
                *loop_,
            );

            // Update our state
            *guard = State::Exited {
                pid,
                status: exit_status,
            };
            shared.state_cv.notify_all();
            taken
        };

        if let (Some(completion), Some(loop_xev)) = (completion, loop_xev) {
            let callback = completion.callback;
            loop_xev.run_soon(Box::new(move || callback(Ok(exit_status))));
        }
        log::debug!(target: "flatpak", "HostCommand exited pid={pid} status={exit_status}");

        // We're done now, so we can unsubscribe
        gio_c::g_dbus_connection_signal_unsubscribe(bus, subscription);

        // We are also done with our loop so we can exit.
        glib_c::g_main_loop_quit(loop_.0);
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn flags_bits() {
            assert_eq!(0, Flags::default().to_bits());
            assert_eq!(
                2,
                Flags {
                    watch_bus: true,
                    ..Default::default()
                }
                .to_bits()
            );
            assert_eq!(
                3,
                Flags {
                    clear_env: true,
                    watch_bus: true
                }
                .to_bits()
            );
        }

        #[test]
        fn wait_before_spawn_fails() {
            let cmd = FlatpakHostCommand::new(vec!["true".into()], 0, 1, 2);
            assert_eq!(Err(Error::FlatpakMustBeStarted), cmd.wait());
            assert_eq!(Ok(()), cmd.signal(15, false));

            struct Inline;
            impl XevLoop for Inline {
                fn run_soon(&self, f: Box<dyn FnOnce() + Send>) {
                    f()
                }
            }
            let (tx, rx) = std::sync::mpsc::channel();
            cmd.wait_xev(
                Arc::new(Inline),
                Completion {
                    callback: Box::new(move |r| tx.send(r).unwrap()),
                },
            );
            assert_eq!(
                Err(WaitError::Flatpak(Error::FlatpakMustBeStarted)),
                rx.recv().unwrap()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn is_flatpak_matches_marker_file() {
        let expected = cfg!(target_os = "linux") && std::path::Path::new("/.flatpak-info").exists();
        assert_eq!(expected, super::is_flatpak());
    }
}
