//! Port of `src/crash/sentry.zig`.
//!
//! STATUS: ported
//!
//! The Sentry native SDK integration is only compiled with the `sentry`
//! Cargo feature (the equivalent of the `build_options.sentry` build flag),
//! which links the system `sentry` C library. Without it, [`init`] and
//! [`deinit`] are no-ops, exactly like the Zig build with sentry disabled.

use std::cell::Cell;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use super::sentry_envelope::{Envelope, EnvelopeError, ItemType};
use crate::os::{xdg, EnvironMap};

// The `sentry` log scope.
#[allow(dead_code)]
const LOG_TARGET: &str = "sentry";

/// Errors from Sentry initialization and crash report persistence.
#[derive(Debug)]
pub enum Error {
    SentryInitFailed,
    StateDirNotInitialized,
    /// The XDG directory lookup failed.
    Xdg(xdg::Error),
    /// The macOS cache directory lookup failed.
    #[cfg(target_os = "macos")]
    AppleAPIFailed,
    Envelope(EnvelopeError),
    Io(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Envelope(err) => write!(f, "{err}"),
            Error::Io(err) => write!(f, "{err}"),
            other => fmt::Debug::fmt(other, f),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self {
        Error::Io(err)
    }
}

impl From<EnvelopeError> for Error {
    fn from(err: EnvelopeError) -> Self {
        Error::Envelope(err)
    }
}

impl From<xdg::Error> for Error {
    fn from(err: xdg::Error) -> Self {
        Error::Xdg(err)
    }
}

#[cfg(target_os = "macos")]
impl From<crate::os::macos::CacheDirError> for Error {
    fn from(err: crate::os::macos::CacheDirError) -> Self {
        match err {
            crate::os::macos::CacheDirError::AppleAPIFailed => Error::AppleAPIFailed,
        }
    }
}

/// Thread-local state that can be set by thread main functions so that
/// crashes have more context.
///
/// This is a hack over Sentry native SDK limitations. The native SDK has
/// one global scope for all threads and no support for thread-local scopes.
/// This means that if we want to set thread-specific data we have to do it
/// on our own in the on crash callback.
#[derive(Clone, Copy)]
pub struct ThreadState {
    /// Thread type, used to tag the crash
    pub r#type: ThreadType,

    /// The surface that this thread is attached to.
    pub surface: *const crate::surface::Surface,
}

impl fmt::Debug for ThreadState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ThreadState")
            .field("type", &self.r#type)
            .field("surface", &(self.surface as *const ()))
            .finish()
    }
}

/// `ThreadState.Type`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadType {
    Main,
    Renderer,
    Io,
}

impl ThreadType {
    /// The Zig tag name, used as the crash tag value.
    pub fn as_str(self) -> &'static str {
        match self {
            ThreadType::Main => "main",
            ThreadType::Renderer => "renderer",
            ThreadType::Io => "io",
        }
    }
}

/// The dimensions recorded in a crash's "Dimensions" context (read from
/// `surface.size`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SurfaceDimensions {
    pub screen_width: u32,
    pub screen_height: u32,
    pub grid_columns: u32,
    pub grid_rows: u32,
    pub cell_width: u32,
    pub cell_height: u32,
}

impl SurfaceDimensions {
    /// The dimensions of a surface size.
    pub fn from_size(size: crate::renderer::Size) -> SurfaceDimensions {
        let grid_size = size.grid();
        SurfaceDimensions {
            screen_width: size.screen.width,
            screen_height: size.screen.height,
            grid_columns: grid_size.columns as u32,
            grid_rows: grid_size.rows as u32,
            cell_width: size.cell.width,
            cell_height: size.cell.height,
        }
    }
}

thread_local! {
    /// See ThreadState. This should only ever be set by the owner of the
    /// thread entry function.
    pub static THREAD_STATE: Cell<Option<ThreadState>> = const { Cell::new(None) };
}

/// Set (or clear) this thread's [`ThreadState`].
pub fn set_thread_state(state: Option<ThreadState>) {
    THREAD_STATE.with(|s| s.set(state));
}

/// This thread's [`ThreadState`], if any.
pub fn thread_state() -> Option<ThreadState> {
    THREAD_STATE.with(|s| s.get())
}

/// Process-wide initialization of our Sentry client.
///
/// This should only be called from one thread, and deinit should be called
/// from the same thread that calls init to avoid data races.
///
/// PRIVACY NOTE: I want to make it very clear that Ghostty by default does
/// NOT send any data over the network. We use the Sentry native SDK to collect
/// crash reports and logs, but we only store them locally (see Transport).
/// It is up to the user to grab the logs and manually send them to us
/// (or to their own Sentry instance) if they want to.
pub fn init(environ_map: EnvironMap) -> Result<(), Error> {
    #[cfg(not(feature = "sentry"))]
    {
        drop(environ_map);
        Ok(())
    }

    #[cfg(feature = "sentry")]
    {
        native::init(environ_map)
    }
}

/// Process-wide deinitialization of our Sentry client. This ensures all
/// our data is flushed.
pub fn deinit() {
    #[cfg(feature = "sentry")]
    native::deinit();
}

#[cfg_attr(not(feature = "sentry"), allow(dead_code))]
fn cache_dir(environ_map: &EnvironMap) -> Result<PathBuf, Error> {
    // On macOS, we prefer to use the NSCachesDirectory value to be
    // a more idiomatic macOS application. But if XDG env vars are set
    // we will respect them.
    #[cfg(target_os = "macos")]
    if let Some(xdg_cache_home) = environ_map.get("XDG_CACHE_HOME") {
        if !xdg_cache_home.is_empty() {
            return Ok(PathBuf::from(crate::os::macos::cache_dir("sentry")?));
        }
    }

    Ok(PathBuf::from(xdg::cache(
        environ_map,
        xdg::Options {
            subdir: Some("ghostty/sentry"),
            ..Default::default()
        },
    )?))
}

/// The crash report transport. Reports are only ever persisted to the
/// local state directory.
pub struct Transport;

impl Transport {
    /// Persist a serialized envelope (`json`) as `<uuid>.ghosttycrash` in
    /// `state_dir`, unless it doesn't contain a crash event.
    ///
    /// This is the part of `sendInternal` that doesn't depend on the
    /// Sentry C library. Returns the path written, or `None` if the
    /// envelope was discarded.
    pub fn persist(state_dir: &Path, json: &[u8], uuid: &str) -> Result<Option<PathBuf>, Error> {
        // Parse into an envelope structure
        let parsed = {
            let mut reader: &[u8] = json;
            Envelope::parse(&mut reader)?
        };

        // If our envelope doesn't have an event then we don't do anything.
        // To figure this out we first encode it into a string, parse it,
        // and check if it has an event. Kind of wasteful but the best
        // option we have at the time of writing this since the C API doesn't
        // expose this information.
        if Self::should_discard(&parsed) {
            log::info!(target: "sentry", "sentry envelope does not contain crash, discarding");
            return Ok(None);
        }

        fs::create_dir_all(state_dir)?;

        // Build our final path and write to it.
        let path = state_dir.join(format!("{uuid}.ghosttycrash"));
        let file = fs::File::create(&path)?;
        let mut file_writer = io::BufWriter::with_capacity(4096, file);
        file_writer.write_all(json)?;
        file_writer.flush()?;

        log::warn!(target: "sentry", "crash report written to disk path={}", path.display());
        Ok(Some(path))
    }

    fn should_discard(envelope: &Envelope) -> bool {
        // If we have an event item then we're good.
        !envelope
            .items
            .iter()
            .any(|item| item.item_type() == ItemType::Event)
    }
}

/// The parts that require the Sentry native SDK C library.
#[cfg(feature = "sentry")]
mod native {
    use super::*;
    use std::ffi::{c_char, c_void};
    use std::sync::{Mutex, OnceLock};
    use std::thread::JoinHandle;

    use crate::build_config;
    use crate::pkg::sentry::c;
    use crate::pkg::sentry::{set_tag, Value};

    /// The global state for the Sentry SDK. This is unavoidable since crash
    /// handling is a global process-wide thing.
    static INIT_THREAD: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

    /// Holds the XDG cache dir. Set once, persistently, by the init thread.
    static CACHE_DIR: OnceLock<PathBuf> = OnceLock::new();

    /// Holds the XDG state dir. Set once, persistently, by the init thread.
    static STATE_DIR: OnceLock<PathBuf> = OnceLock::new();

    pub(super) fn init(environ_map: EnvironMap) -> Result<(), Error> {
        let mut init_thread = INIT_THREAD.lock().unwrap_or_else(|e| e.into_inner());

        // Must only start once
        debug_assert!(init_thread.is_none());

        // We use a thread for initializing Sentry because initialization is
        // slow enough to matter for process startup: resolving our directories
        // can take multiple milliseconds on macOS (Apple APIs) and Sentry's
        // own init does disk I/O. Everything Sentry is doing initially is safe
        // to do on a separate thread and fast enough that its very likely to
        // be done before a crash occurs.
        //
        // The environ map is a snapshot owned by the thread (and freed there),
        // so it is safe against concurrent mutations of the process environment.
        let thr = std::thread::Builder::new()
            .name("sentry-init".to_owned())
            .spawn(move || {
                if let Err(err) = init_thread_main(environ_map) {
                    log::warn!(target: "sentry", "sentry init failed err={err}");
                }
            })?;

        *init_thread = Some(thr);
        Ok(())
    }

    fn init_thread_main(environ_map: EnvironMap) -> Result<(), Error> {
        // Get our directories.
        let state_dir = crate::crash::dir::default_dir(&environ_map)?.path;
        let _ = STATE_DIR.set(state_dir);

        let cache_dir = cache_dir(&environ_map)?;
        let cache_dir = CACHE_DIR.get_or_init(|| cache_dir);

        // SAFETY: all FFI calls below are given valid pointers; `opts` is
        // owned by us until `sentry_init` takes ownership of it.
        unsafe {
            let transport = c::sentry_transport_new(Some(send));
            // This will crash if the transport was never used so we avoid
            // that for now. This probably leaks some memory but it'd be very
            // small and a one time cost. Once this is fixed upstream we can
            // remove this.

            let opts = c::sentry_options_new();
            c::sentry_options_set_release_n(
                opts,
                build_config::VERSION_STRING.as_ptr().cast(),
                build_config::VERSION_STRING.len(),
            );
            c::sentry_options_set_transport(opts, transport);

            // Set our crash callback. See beforeSend for more details on what we
            // do here and why we use this.
            c::sentry_options_set_before_send(opts, Some(before_send), std::ptr::null_mut());

            use std::os::unix::ffi::OsStrExt;
            let db = cache_dir.as_os_str().as_bytes();
            c::sentry_options_set_database_path_n(opts, db.as_ptr().cast(), db.len());

            if cfg!(debug_assertions) {
                // Debug logging for Sentry
                c::sentry_options_set_debug(opts, 1);
            }

            // Initialize (sentry_init takes ownership of opts, even on failure)
            if c::sentry_init(opts) != 0 {
                return Err(Error::SentryInitFailed);
            }
        }

        // Setup some basic tags that we always want present
        set_tag("build-mode", build_config::MODE_STRING);
        set_tag("app-runtime", build_config::APP_RUNTIME.as_str());
        set_tag("font-backend", build_config::FONT_BACKEND.as_str());
        set_tag("renderer", build_config::RENDERER.as_str());

        // Log some information about sentry
        log::debug!(target: "sentry", "sentry initialized database={}", cache_dir.display());
        Ok(())
    }

    pub(super) fn deinit() {
        // If we're still initializing then wait for init to finish. This
        // is highly unlikely since init is a very fast operation but we want
        // to avoid the possibility.
        let thr = INIT_THREAD.lock().unwrap_or_else(|e| e.into_inner()).take();
        let Some(thr) = thr else { return };
        let _ = thr.join();
        // SAFETY: plain FFI call.
        unsafe {
            c::sentry_close();
        }
    }

    unsafe extern "C" fn before_send(
        event_val: c::sentry_value_t,
        _: *mut c_void,
        _: *mut c_void,
    ) -> c::sentry_value_t {
        // The native SDK at the time of writing doesn't support thread-local
        // scopes. The full SDK has one global scope. So we use the beforeSend
        // handler to set thread-specific data such as window size, grid size,
        // etc. that we can use to debug crashes.

        // If we don't have thread state we can't reliably determine
        // metadata such as surface dimensions. In the future we can probably
        // drop full app state (all surfaces, all windows, etc.).
        let Some(thr_state) = thread_state() else {
            log::debug!(target: "sentry", "no thread state, skipping crash metadata");
            return event_val;
        };

        // Get our event contexts. At this point Sentry has already merged
        // all the contexts so we should have this key. If not, we create it.
        let event = Value { value: event_val };
        let contexts = event.get("contexts").unwrap_or_else(|| {
            let obj = Value::init_object();
            event.set("contexts", obj);
            obj
        });
        let tags = event.get("tags").unwrap_or_else(|| {
            let obj = Value::init_object();
            event.set("tags", obj);
            obj
        });

        // Store our thread type
        tags.set("thread-type", Value::init_string(thr_state.r#type.as_str()));

        // Read the surface data. This is likely unsafe because on a crash
        // other threads can continue running. We don't have race-safe way to
        // access this data so this might be corrupted but it's most likely fine.
        {
            let obj = Value::init_object();
            // SAFETY: the thread state owner guarantees the surface outlives
            // the time the state is set.
            // (A null surface is only possible in tests; report nothing.)
            let dims = if thr_state.surface.is_null() {
                SurfaceDimensions::default()
            } else {
                SurfaceDimensions::from_size(unsafe { (*thr_state.surface).size })
            };
            let cast = |v: u32| i32::try_from(v).unwrap_or(-1);
            obj.set("screen-width", Value::init_int32(cast(dims.screen_width)));
            obj.set("screen-height", Value::init_int32(cast(dims.screen_height)));
            obj.set("grid-columns", Value::init_int32(cast(dims.grid_columns)));
            obj.set("grid-rows", Value::init_int32(cast(dims.grid_rows)));
            obj.set("cell-width", Value::init_int32(cast(dims.cell_width)));
            obj.set("cell-height", Value::init_int32(cast(dims.cell_height)));

            contexts.set("Dimensions", obj);
        }

        event_val
    }

    /// `Transport.send`
    unsafe extern "C" fn send(envelope: *mut c::sentry_envelope_t, _ud: *mut c_void) {
        // Call our internal impl. If it fails there is nothing we can do
        // but log to the user.
        // SAFETY: sentry hands us a valid envelope that we own.
        if let Err(err) = unsafe { send_internal(envelope) } {
            log::warn!(target: "sentry", "failed to persist crash report err={err}");
        }
        // SAFETY: we own the envelope.
        unsafe { c::sentry_envelope_free(envelope) };
    }

    /// Implementation of send but we can use Rust errors.
    unsafe fn send_internal(envelope: *mut c::sentry_envelope_t) -> Result<(), Error> {
        let state_dir = STATE_DIR.get().ok_or(Error::StateDirNotInitialized)?;

        // Serialize the envelope.
        let mut len: usize = 0;
        // SAFETY: valid envelope; the returned buffer is freed below.
        let ptr = unsafe { c::sentry_envelope_serialize(envelope, &mut len) };
        if ptr.is_null() {
            return Err(Error::Io(io::Error::other(
                "sentry_envelope_serialize failed",
            )));
        }
        struct Free(*mut c_char);
        impl Drop for Free {
            fn drop(&mut self) {
                // SAFETY: allocated by sentry.
                unsafe { c::sentry_free(self.0.cast()) }
            }
        }
        let _free = Free(ptr);
        // SAFETY: sentry returned `len` valid bytes at `ptr`.
        let json = unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), len) };

        // Generate a UUID for this envelope. The envelope DOES have an event_id
        // header but I don't think there is any public API way to get it
        // afaict so we generate a new UUID for the filename just so we don't
        // conflict.
        let uuid = {
            // SAFETY: plain FFI calls with a 37-byte output buffer.
            let uuid = unsafe { c::sentry_uuid_new_v4() };
            let mut buf = [0 as c_char; 37];
            unsafe { c::sentry_uuid_as_string(&uuid, buf.as_mut_ptr()) };
            let bytes: Vec<u8> = buf[..36].iter().map(|&c| c as u8).collect();
            String::from_utf8_lossy(&bytes).into_owned()
        };

        Transport::persist(state_dir, json, &uuid)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TmpDir {
        path: PathBuf,
    }

    impl TmpDir {
        fn new() -> TmpDir {
            let mut rand = [0u8; 8];
            getrandom::fill(&mut rand).unwrap();
            let name: String = rand.iter().map(|b| format!("{b:02x}")).collect();
            let path = std::env::temp_dir().join(format!("ghostty-crash-sentry-test-{name}"));
            TmpDir { path }
        }
    }

    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn transport_discards_envelope_without_event() {
        let tmp = TmpDir::new();
        let json = concat!("{}\n", "{\"type\":\"session\",\"length\":2}\n", "{}",);
        let result = Transport::persist(&tmp.path, json.as_bytes(), "uuid").unwrap();
        assert!(result.is_none());
        assert!(!tmp.path.exists());
    }

    #[test]
    fn transport_persists_envelope_with_event() {
        let tmp = TmpDir::new();
        let json = concat!(
            "{\"event_id\":\"abc\"}\n",
            "{\"type\":\"event\",\"length\":2}\n",
            "{}\n",
            "{\"type\":\"attachment\",\"length\":4,\"filename\":\"test.txt\"}\n",
            "ABCD",
        );
        let path = Transport::persist(&tmp.path, json.as_bytes(), "1234")
            .unwrap()
            .unwrap();
        assert_eq!(tmp.path.join("1234.ghosttycrash"), path);
        assert_eq!(json.as_bytes(), &fs::read(&path).unwrap()[..]);

        // The report shows up in the crash dir iterator.
        let dir = crate::crash::dir::Dir {
            path: tmp.path.clone(),
        };
        let mut it = dir.iterator();
        let report = it.next().unwrap().unwrap();
        assert_eq!("1234.ghosttycrash", report.name);
        assert!(it.next().unwrap().is_none());
    }

    #[test]
    fn transport_malformed_envelope() {
        let tmp = TmpDir::new();
        assert!(Transport::persist(&tmp.path, b"[]", "uuid").is_err());
    }

    #[test]
    fn thread_state_is_thread_local() {
        // The surface is only an identifier here (the real surface is
        // exercised by the surface integration test).
        let s = std::ptr::NonNull::<crate::surface::Surface>::dangling().as_ptr();
        assert!(thread_state().is_none());
        set_thread_state(Some(ThreadState {
            r#type: ThreadType::Renderer,
            surface: s,
        }));
        let st = thread_state().unwrap();
        assert_eq!(ThreadType::Renderer, st.r#type);
        assert_eq!("renderer", st.r#type.as_str());
        assert_eq!(s as *const _, st.surface);
        let dims = SurfaceDimensions::from_size(crate::renderer::Size {
            screen: crate::renderer::ScreenSize {
                width: 800,
                height: 600,
            },
            cell: crate::renderer::CellSize {
                width: 10,
                height: 20,
            },
            padding: Default::default(),
        });
        assert_eq!(800, dims.screen_width);
        assert_eq!(80, dims.grid_columns);
        assert_eq!(30, dims.grid_rows);
        std::thread::spawn(|| assert!(thread_state().is_none()))
            .join()
            .unwrap();
        set_thread_state(None);
        assert!(thread_state().is_none());
    }

    #[test]
    fn init_deinit_without_sentry() {
        // Without the `sentry` feature these are no-ops.
        #[cfg(not(feature = "sentry"))]
        {
            init(EnvironMap::new()).unwrap();
            deinit();
        }
    }

    #[test]
    fn cache_dir_path() {
        let mut env = EnvironMap::new();
        env.insert("XDG_CACHE_HOME".into(), "/cache".into());
        assert_eq!(
            PathBuf::from("/cache/ghostty/sentry"),
            cache_dir(&env).unwrap()
        );
    }
}
