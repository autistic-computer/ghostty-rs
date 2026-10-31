//! Port of `src/apprt/embedded.zig`.
//!
//! STATUS: ported
//!
//! Application runtime for the embedded version of Ghostty. The embedded
//! version is when Ghostty is embedded within a parent host application,
//! rather than owning the application lifecycle itself. This is used for
//! example for the macOS build of Ghostty so that we can use a native
//! Swift+XCode-based application.
//!
//! Rust notes:
//! * The embedded runtime is the selected `apprt::{App, Surface}` on macOS
//!   (non-test builds; see `apprt.rs`). The few operations that hand this
//!   runtime's objects to the core as *the* runtime objects (registering a
//!   surface with the core app, creating the core surface, app-level key
//!   events, ticking the core app, ...) only type-check there, so they are
//!   gated with `#[cfg(all(target_os = "macos", not(test)))]`; everything
//!   else (the C ABI types, the clipboard/key/mouse glue and most of the
//!   `ghostty_*` exports) also builds on Linux.
//! * Apple-only parts (`objc` views, Metal, CoreText, `CGS*`) are gated with
//!   `#[cfg(target_os = "macos")]`, and the Dear ImGui parts of
//!   [`Inspector`] with the `inspector` Cargo feature (the Rust `dcimgui`
//!   package only exists with it): without it, `ghostty_surface_inspector`
//!   fails (returns null) and the other inspector calls are no-ops.
//! * Zig's `App.Options`, `App.KeyEvent`, `App.KeyTarget`,
//!   `Surface.Options`, `Platform.C`/`MacOS`/`IOS` and `Inspector.Backend`
//!   are the module-level [`Options`], [`KeyEvent`], [`KeyTarget`],
//!   [`SurfaceOptions`], [`PlatformC`]/[`PlatformMacOS`]/[`PlatformIOS`]
//!   and [`InspectorBackend`]; the `CAPI` namespace is the [`capi`] module.
//! * The Zig surface embeds its `CoreSurface` by value; the Rust core
//!   surface is created boxed by `CoreSurface::init`, so [`Surface`] holds
//!   it as a raw pointer (like the GTK runtime).
//! * The core calls `perform_action` through a shared reference, so the app
//!   configuration lives in an [`AppConfig`] cell (it derefs to `Config`).
//! * Clipboard request states cross the C API as `Box::into_raw` pointers
//!   (Zig: allocator-created `*apprt.ClipboardRequest`).
//! * `ghostty_inspector_metal_shutdown` returns `bool` (`true`) as declared
//!   by `ghostty.h` (the Zig export returns `void`).
//! * `ghostty_surface_quicklook_font` takes the font grid's write lock
//!   (Zig: shared lock) because loading a face mutates the collection.

use std::cell::{RefCell, UnsafeCell};
use std::ffi::{c_char, c_int, c_uint, c_void, CStr, CString};
use std::fmt;
use std::ops::Deref;

use crate::app::App as CoreApp;
use crate::apprt;
use crate::apprt::action::{Action, Target, TargetC, ActionC};
use crate::apprt::surface::NewSurfaceContext;
use crate::apprt::{
    Clipboard, ClipboardContent, ClipboardReadResult, ClipboardRequest, ColorScheme, ContentScale,
    CursorPos, SurfaceSize,
};
use crate::command::EnvMap;
use crate::config::Config;
use crate::input;
use crate::lib::tinyio::{Mutex, TinyIo};
use crate::renderer;
use crate::surface::{self as core_surface, CompleteClipboard, InputEffect, Surface as CoreSurface};
use crate::terminal;

#[cfg(target_os = "macos")]
use crate::renderer::metal::objc;

const LOG: &str = "embedded_window";

pub use crate::os::resources_dir;

/// Errors from the embedded runtime (Zig: the inferred error sets).
#[derive(Debug)]
pub enum Error {
    /// A platform configuration error.
    Platform(PlatformError),
    /// The core app couldn't be created.
    CreateApp(crate::app::CreateError),
    /// A core surface error.
    Surface(core_surface::Error),
    /// The inspector (Dear ImGui) isn't available in this build or its
    /// context couldn't be created.
    Inspector,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Platform(e) => fmt::Display::fmt(e, f),
            Error::CreateApp(e) => write!(f, "{e:?}"),
            Error::Surface(e) => fmt::Display::fmt(e, f),
            Error::Inspector => f.write_str("OutOfMemory"),
        }
    }
}

impl std::error::Error for Error {}

impl From<PlatformError> for Error {
    fn from(e: PlatformError) -> Self {
        Error::Platform(e)
    }
}

impl From<core_surface::Error> for Error {
    fn from(e: core_surface::Error) -> Self {
        Error::Surface(e)
    }
}

/// Because we only expect the embedding API to be used in embedded
/// environments, the options are extern so that we can expose it
/// directly to a C callconv and not pay for any translation costs.
///
/// C type: ghostty_runtime_config_s (Zig: `App.Options`)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// Userdata that is passed to all the callbacks.
    pub userdata: *mut c_void,

    /// True if the selection clipboard is supported.
    pub supports_selection_clipboard: bool,

    /// Callback called to wakeup the event loop. This should trigger
    /// a full tick of the app loop.
    pub wakeup: extern "C" fn(*mut c_void),

    /// Callback called to handle an action.
    pub action: extern "C" fn(*mut App, TargetC, ActionC) -> bool,

    /// Read the clipboard value. The result only reports facts about
    /// the clipboard: whether the request was started (in which case
    /// complete_clipboard_request or deny_clipboard_request must
    /// eventually be called with the given state pointer), whether the
    /// clipboard has no servable contents, or whether the clipboard
    /// can't be read at all. How each non-started state is answered is
    /// up to the core.
    ///
    /// The MIME types are exactly the representations the caller wants
    /// served; the embedder should read only those. Text-like types
    /// are always requested as the canonical "text/plain". The final
    /// bool asks for the listing of all MIME types available on the
    /// clipboard to be delivered with the completion.
    pub read_clipboard: extern "C" fn(
        *mut c_void,
        c_int,
        *mut ClipboardRequest,
        *const *const c_char,
        usize,
        bool,
    ) -> ClipboardReadResult,

    /// This may be called after a read clipboard call to request
    /// confirmation that the clipboard value is safe to read. The
    /// embedder must call complete_clipboard_request (usually with
    /// the confirmation's contents, which are only borrowed for
    /// this call) or deny_clipboard_request with the given request.
    ///
    /// The last argument is the `ghostty_clipboard_request_e` tag.
    pub confirm_read_clipboard: extern "C" fn(
        *mut c_void,
        *const capi::ClipboardConfirm,
        *mut ClipboardRequest,
        c_int,
    ),

    /// Write the clipboard value.
    pub write_clipboard: extern "C" fn(*mut c_void, c_int, *const capi::ClipboardContent, usize, bool),

    /// Close the current surface given by this function.
    pub close_surface: Option<extern "C" fn(*mut c_void, bool)>,
}

/// This is the key event sent for ghostty_surface_key and
/// ghostty_app_key. (Zig: `App.KeyEvent`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent<'a> {
    pub action: input::Action,
    pub mods: input::Mods,
    pub consumed_mods: input::Mods,
    pub keycode: u32,
    pub text: Option<&'a [u8]>,
    pub unshifted_codepoint: u32,
    pub composing: bool,
}

impl<'a> KeyEvent<'a> {
    /// Convert a libghostty key event into a core key event.
    pub fn core(&self) -> Option<input::KeyEvent<'a>> {
        let text: &'a [u8] = self.text.unwrap_or(b"");

        // Zig: `std.math.cast(u21, ...) orelse 0`.
        let unshifted_codepoint: u32 = if self.unshifted_codepoint <= 0x1F_FFFF {
            self.unshifted_codepoint
        } else {
            0
        };

        // We want to get the physical unmapped key to process keybinds.
        let physical_key = input::keycodes::ENTRIES
            .iter()
            .find(|entry| entry.native == self.keycode)
            .map(|entry| entry.key)
            .unwrap_or(input::Key::Unidentified);

        // Build our final key event
        Some(input::KeyEvent {
            action: self.action,
            key: physical_key,
            mods: self.mods,
            consumed_mods: self.consumed_mods,
            composing: self.composing,
            utf8: text,
            unshifted_codepoint,
        })
    }
}

/// The target of a key event. This is used to determine some subtly
/// different behavior between app and surface key events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyTarget {
    App,
    Surface(*mut Surface),
}

/// The keymap error type of the current platform.
#[cfg(target_os = "macos")]
pub type KeymapError = crate::input::keymap_darwin::Error;
/// The keymap error type of the current platform.
#[cfg(not(target_os = "macos"))]
pub type KeymapError = std::convert::Infallible;

/// The configuration for the app (Zig: `App.config`). The Zig runtime
/// replaces it from `performAction`, which the Rust core calls through a
/// shared reference, so the value lives in an `UnsafeCell`. It derefs to
/// the [`Config`].
pub struct AppConfig(UnsafeCell<Config>);

impl AppConfig {
    fn new(config: Config) -> AppConfig {
        AppConfig(UnsafeCell::new(config))
    }

    /// Replace the configuration.
    ///
    /// # Safety
    ///
    /// No reference into the previous configuration may be live.
    unsafe fn replace(&self, config: Config) {
        // SAFETY: forwarded from the caller (the app is only used on the
        // main thread).
        unsafe { *self.0.get() = config };
    }

    fn get_mut(&mut self) -> &mut Config {
        self.0.get_mut()
    }
}

impl Deref for AppConfig {
    type Target = Config;

    fn deref(&self) -> &Config {
        // SAFETY: the config is only replaced while no reference is live
        // (see `replace`).
        unsafe { &*self.0.get() }
    }
}

impl fmt::Debug for AppConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AppConfig")
    }
}

pub struct App {
    pub core_app: *mut CoreApp,
    pub opts: Options,

    /// The keyboard layout keymap. This is lazily initialized on first
    /// use because creating it requires talking to the text input
    /// system (TIS on macOS), and the first such call in a process is
    /// slow (multiple milliseconds). It is only needed once keyboard
    /// events start flowing, at which point the system is warm.
    pub keymap: RefCell<Option<input::Keymap>>,

    /// The configuration for the app. This is owned by this structure.
    pub config: AppConfig,
}

impl fmt::Debug for App {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("App")
            .field("core_app", &self.core_app)
            .field("opts", &self.opts)
            .finish_non_exhaustive()
    }
}

// SAFETY: like the Zig runtime, the app is used on the main thread; other
// threads only call `wakeup`, whose callback the embedder makes
// thread-safe.
unsafe impl Send for App {}
unsafe impl Sync for App {}

impl App {
    pub fn init(core_app: *mut CoreApp, config: &Config, opts: Options) -> Result<App, Error> {
        // We have to clone the config.
        let config_clone = config.clone();

        Ok(App {
            core_app,
            config: AppConfig::new(config_clone),
            opts,
            keymap: RefCell::new(None),
        })
    }

    pub fn terminate(&mut self) {
        if let Some(v) = self.keymap.get_mut().take() {
            v.deinit();
        }
        self.config.get_mut().deinit();
    }

    fn core_app(&self) -> &mut CoreApp {
        // SAFETY: the core app outlives the runtime app and is only used
        // on the main thread.
        unsafe { &mut *self.core_app }
    }

    /// Returns true if there are any global keybinds in the configuration.
    pub fn has_global_keybinds(&self) -> bool {
        use crate::input::binding::Value;
        for value in self.config.keybind.set.bindings.values() {
            match value {
                Value::Leader(_) => {}
                Value::Leaf(leaf) => {
                    if leaf.flags.global {
                        return true;
                    }
                }
                Value::LeafChained(leaf) => {
                    if leaf.flags.global {
                        return true;
                    }
                }
            }
        }

        false
    }

    /// See CoreApp.focusEvent
    pub fn focus_event(&self, focused: bool) {
        self.core_app().focus_event(focused);
    }

    /// See CoreApp.keyEvent.
    #[cfg(all(target_os = "macos", not(test)))]
    pub fn key_event(&self, target: KeyTarget, event: KeyEvent<'_>) -> Result<bool, Error> {
        // Convert our C key event into a Zig one.
        let Some(input_event) = event.core() else {
            return Ok(false);
        };

        // Invoke the core Ghostty logic to handle this input.
        let effect: InputEffect = match target {
            KeyTarget::App => {
                if self.core_app().key_event(self, input_event) {
                    InputEffect::Consumed
                } else {
                    InputEffect::Ignored
                }
            }

            // SAFETY: the target surface is live (C API contract).
            KeyTarget::Surface(surface) => unsafe { (*surface).core_mut() }.key_callback(input_event)?,
        };

        Ok(match effect {
            InputEffect::Closed => true,
            InputEffect::Ignored => false,
            InputEffect::Consumed => true,
        })
    }

    /// This should be called whenever the keyboard layout was changed.
    pub fn reload_keymap(&self) -> Result<(), KeymapError> {
        // Reload the keymap. If it was never initialized we don't need
        // to do anything since lazy initialization will pick up the
        // current layout.
        if let Some(v) = self.keymap.borrow_mut().as_mut() {
            v.reload()?;
        }
        Ok(())
    }

    /// Loads the keyboard layout.
    ///
    /// Kind of expensive so this should be avoided if possible. When I say
    /// "kind of expensive" I mean that its not something you probably want
    /// to run on every keypress.
    pub fn keyboard_layout(&self) -> input::KeyboardLayout {
        // We only support keyboard layout detection on macOS.
        #[cfg(not(target_os = "macos"))]
        {
            input::KeyboardLayout::Unknown
        }

        #[cfg(target_os = "macos")]
        {
            // Lazily initialize the keymap.
            let mut keymap = self.keymap.borrow_mut();
            if keymap.is_none() {
                match input::Keymap::init() {
                    Ok(v) => *keymap = Some(v),
                    Err(err) => {
                        log::warn!(target: LOG, "error initializing keymap err={err:?}");
                        return input::KeyboardLayout::Unknown;
                    }
                }
            }
            let keymap = keymap.as_ref().expect("initialized above");

            // Any layout larger than this is not something we can handle.
            let mut buf = [0u8; 256];
            let Ok(id) = keymap.source_id(&mut buf) else {
                return input::KeyboardLayout::Unknown;
            };

            input::KeyboardLayout::map_apple_id(id).unwrap_or(input::KeyboardLayout::Unknown)
        }
    }

    pub fn wakeup(&self) {
        (self.opts.wakeup)(self.opts.userdata);
    }

    pub fn wait(&self) -> Result<(), Error> {
        Ok(())
    }

    /// Create a new surface for the app.
    #[cfg(all(target_os = "macos", not(test)))]
    fn new_surface(&mut self, opts: &SurfaceOptions) -> Result<*mut Surface, Error> {
        // Create the surface. Its address must be stable before init
        // because the core holds on to it.
        let surface = Box::into_raw(Box::new(Surface::new(self, opts)?));
        // SAFETY: just allocated; freed below on failure.
        if let Err(err) = unsafe { Surface::init(surface, self, opts) } {
            // SAFETY: init cleaned up after itself; free the allocation.
            drop(unsafe { Box::from_raw(surface) });
            return Err(err);
        }

        Ok(surface)
    }

    /// Close the given surface.
    #[cfg(all(target_os = "macos", not(test)))]
    pub fn close_surface(&mut self, surface: *mut Surface) {
        // SAFETY: the surface was created by `new_surface` and is freed
        // exactly once (C API contract).
        unsafe {
            (*surface).deinit();
            drop(Box::from_raw(surface));
        }
    }

    pub fn redraw_inspector(&self, surface: &Surface) {
        surface.queue_inspector_render();
    }

    /// Perform a given action. Returns `true` if the action was able to be
    /// performed, `false` otherwise.
    pub fn perform_action(&self, target: Target, action: Action<'_>) -> Result<bool, apprt::Error> {
        // Special case certain actions before they are sent to the
        // embedded apprt.
        self.perform_pre_action(target, &action);

        log::debug!(target: LOG, "dispatching action target={target:?} action={:?}", action.key());
        Ok((self.opts.action)(
            self as *const App as *mut App,
            target.cval(),
            action.cval(),
        ))
    }

    fn perform_pre_action(&self, target: Target, action: &Action<'_>) {
        // Special case certain actions before they are sent to the embedder
        match action {
            Action::SetTitle(value) => match target {
                Target::App => {}
                Target::Surface(_surface) => {
                    // Dupe the title so that we can store it.
                    #[cfg(all(target_os = "macos", not(test)))]
                    // SAFETY: a surface target is a live core surface whose
                    // runtime surface is one of ours.
                    unsafe {
                        let rt_surface: *mut Surface = (*_surface).rt_surface;
                        (*rt_surface).title = Some(value.title.to_owned());
                    }
                    #[cfg(not(all(target_os = "macos", not(test))))]
                    let _ = value;
                }
            },

            Action::ConfigChange(value) => match target {
                Target::Surface(_) => {}

                // For app updates, we update our core config. We need to
                // clone it because the caller owns the param.
                // SAFETY: the core holds no reference into the app config
                // while it reports a change (it passes its own config).
                Target::App => unsafe { self.config.replace(value.config.clone()) },
            },

            _ => {}
        }
    }

    /// Send the given IPC to a running Ghostty. Returns `true` if the action was
    /// able to be performed, `false` otherwise.
    ///
    /// Note that this is a static function. Since this is called from a CLI app (or
    /// some other process that is not Ghostty) there is no full-featured apprt App
    /// to use.
    pub fn perform_ipc(
        _target: &apprt::ipc::Target<'_>,
        action: &apprt::ipc::Action<'_>,
    ) -> Result<bool, apprt::ipc::Errors> {
        match action {
            apprt::ipc::Action::NewWindow(_) => Ok(false),
            apprt::ipc::Action::NewTab(_) => Ok(false),
            apprt::ipc::Action::ToggleQuickTerminal => Ok(false),
        }
    }
}

/// Platform-specific configuration for libghostty.
#[derive(Debug, Clone, Copy)]
pub enum Platform {
    Macos(PlatformMacOS),
    Ios(PlatformIOS),
}

/// `Platform.MacOS`
#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy)]
pub struct PlatformMacOS {
    /// The view to render the surface on.
    pub nsview: objc::Object,
}

/// `Platform.IOS`
#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy)]
pub struct PlatformIOS {
    /// The view to render the surface on.
    pub uiview: objc::Object,
}

// If our build target for libghostty is not darwin then we do
// not include macos support at all.
/// `Platform.MacOS` (void: unsupported on this target)
#[cfg(not(target_os = "macos"))]
pub type PlatformMacOS = ();
/// `Platform.IOS` (void: unsupported on this target)
#[cfg(not(target_os = "macos"))]
pub type PlatformIOS = ();

/// `Platform.C.macos`
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PlatformCMacOS {
    pub nsview: *mut c_void,
}

/// `Platform.C.ios`
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PlatformCIOS {
    pub uiview: *mut c_void,
}

/// The C ABI compatible version of this union. The tag is expected
/// to be stored elsewhere. (ghostty_platform_u)
#[repr(C)]
#[derive(Clone, Copy)]
pub union PlatformC {
    pub macos: PlatformCMacOS,
    pub ios: PlatformCIOS,
}

impl fmt::Debug for PlatformC {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PlatformC")
    }
}

/// Errors from [`Platform::init`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformError {
    InvalidEnumTag,
    NSViewMustBeSet,
    UIViewMustBeSet,
    UnsupportedPlatform,
}

impl fmt::Display for PlatformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for PlatformError {}

impl Platform {
    /// Initialize a Platform a tag and configuration from the C ABI.
    pub fn init(tag_int: c_int, c_platform: PlatformC) -> Result<Platform, PlatformError> {
        let tag = PlatformTag::from_int(tag_int).ok_or(PlatformError::InvalidEnumTag)?;
        match tag {
            PlatformTag::Macos => {
                #[cfg(target_os = "macos")]
                {
                    // SAFETY: the tag says the macos member is active.
                    let config = unsafe { c_platform.macos };
                    if config.nsview.is_null() {
                        return Err(PlatformError::NSViewMustBeSet);
                    }
                    Ok(Platform::Macos(PlatformMacOS {
                        nsview: objc::Object::from_id(config.nsview),
                    }))
                }
                #[cfg(not(target_os = "macos"))]
                {
                    let _ = c_platform;
                    Err(PlatformError::UnsupportedPlatform)
                }
            }

            PlatformTag::Ios => {
                #[cfg(target_os = "macos")]
                {
                    // SAFETY: the tag says the ios member is active.
                    let config = unsafe { c_platform.ios };
                    if config.uiview.is_null() {
                        return Err(PlatformError::UIViewMustBeSet);
                    }
                    Ok(Platform::Ios(PlatformIOS {
                        uiview: objc::Object::from_id(config.uiview),
                    }))
                }
                #[cfg(not(target_os = "macos"))]
                {
                    let _ = c_platform;
                    Err(PlatformError::UnsupportedPlatform)
                }
            }
        }
    }
}

/// ghostty_platform_e
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformTag {
    // "0" is reserved for invalid so we can detect unset values
    // from the C API.
    Macos = 1,
    Ios = 2,
}

impl PlatformTag {
    /// Zig `std.enums.fromInt`.
    pub fn from_int(v: c_int) -> Option<PlatformTag> {
        match v {
            1 => Some(PlatformTag::Macos),
            2 => Some(PlatformTag::Ios),
            _ => None,
        }
    }
}

/// ghostty_env_var_s
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct EnvVar {
    /// The name of the environment variable.
    pub key: *const c_char,

    /// The value of the environment variable.
    pub value: *const c_char,
}

/// Surface initialization options. (ghostty_surface_config_s, Zig:
/// `Surface.Options`)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SurfaceOptions {
    /// The platform that this surface is being initialized for and
    /// the associated platform-specific configuration.
    pub platform_tag: c_int,
    pub platform: PlatformC,

    /// Userdata passed to some of the callbacks.
    pub userdata: *mut c_void,

    /// The scale factor of the screen.
    pub scale_factor: f64,

    /// The font size to inherit. If 0, default font size will be used.
    pub font_size: f32,

    /// The working directory to load into.
    pub working_directory: *const c_char,

    /// The command to run in the new surface. If this is set then
    /// the "wait-after-command" option is also automatically set to true,
    /// since this is used for scripting.
    ///
    /// This command always run in a shell (e.g. via `/bin/sh -c`),
    /// despite Ghostty allowing directly executed commands via config.
    /// This is a legacy thing and we should probably change it in the
    /// future once we have a concrete use case.
    pub command: *const c_char,

    /// Extra environment variables to set for the surface.
    pub env_vars: *mut EnvVar,
    pub env_var_count: usize,

    /// Input to send to the command after it is started.
    pub initial_input: *const c_char,

    /// Wait after the command exits
    pub wait_after_command: bool,

    /// Context for the new surface
    pub context: NewSurfaceContext,
}

impl Default for SurfaceOptions {
    fn default() -> Self {
        SurfaceOptions {
            platform_tag: 0,
            // Zig: `undefined`.
            platform: PlatformC {
                macos: PlatformCMacOS {
                    nsview: std::ptr::null_mut(),
                },
            },
            userdata: std::ptr::null_mut(),
            scale_factor: 1.0,
            font_size: 0.0,
            working_directory: std::ptr::null(),
            command: std::ptr::null(),
            env_vars: std::ptr::null_mut(),
            env_var_count: 0,
            initial_input: std::ptr::null(),
            wait_after_command: false,
            context: NewSurfaceContext::Window,
        }
    }
}

/// The bytes of a C string, or `None` for a null pointer.
///
/// # Safety
///
/// `ptr` must be null or a valid NUL-terminated string that outlives `'a`.
unsafe fn c_bytes<'a>(ptr: *const c_char) -> Option<&'a [u8]> {
    if ptr.is_null() {
        None
    } else {
        // SAFETY: forwarded from the caller.
        Some(unsafe { CStr::from_ptr(ptr) }.to_bytes())
    }
}

/// A slice from a C pointer and length (empty for a zero length or a null
/// pointer).
///
/// # Safety
///
/// For a non-zero length, `ptr` must point to `len` valid elements that
/// outlive `'a`.
unsafe fn c_slice<'a, T>(ptr: *const T, len: usize) -> &'a [T] {
    if len == 0 || ptr.is_null() {
        &[]
    } else {
        // SAFETY: forwarded from the caller.
        unsafe { std::slice::from_raw_parts(ptr, len) }
    }
}

/// Zig `std.zig.stringEscape`: escape bytes so that they form the
/// contents of a valid Zig string literal (which is what the `raw` input
/// config value expects).
pub fn string_escape(bytes: &[u8], out: &mut Vec<u8>) {
    for &byte in bytes {
        match byte {
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\t' => out.extend_from_slice(b"\\t"),
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'"' => out.extend_from_slice(b"\\\""),
            b'\'' => out.push(b'\''),
            b' ' | b'!' | b'#'..=b'&' | b'('..=b'[' | b']'..=b'~' => out.push(byte),
            _ => {
                const HEX: &[u8; 16] = b"0123456789abcdef";
                out.extend_from_slice(b"\\x");
                out.push(HEX[(byte >> 4) as usize]);
                out.push(HEX[(byte & 0xF) as usize]);
            }
        }
    }
}

/// Holds the renderer state mutex of a core surface until dropped.
struct RendererLock(*const Mutex);

impl RendererLock {
    fn new(core: &CoreSurface) -> RendererLock {
        let m = core.renderer_state.mutex.as_ptr() as *const Mutex;
        // SAFETY: the mutex lives as long as the surface.
        unsafe { (*m).lock_uncancelable(TinyIo) };
        RendererLock(m)
    }
}

impl Drop for RendererLock {
    fn drop(&mut self) {
        // SAFETY: locked in `new`; the mutex outlives the guard.
        unsafe { (*self.0).unlock(TinyIo) };
    }
}

#[derive(Debug)]
pub struct Surface {
    pub app: *mut App,
    pub platform: Platform,
    pub userdata: *mut c_void,
    /// The core surface (Zig: embedded by value). Null until `init`
    /// creates it.
    pub core_surface: *mut CoreSurface,
    pub content_scale: ContentScale,
    pub size: SurfaceSize,
    pub cursor_pos: CursorPos,
    pub inspector: Option<Box<Inspector>>,

    /// The current title of the surface. The embedded apprt saves this so
    /// that getTitle works without the implementer needing to save it.
    pub title: Option<CString>,
}

// SAFETY: see `App`.
unsafe impl Send for Surface {}
unsafe impl Sync for Surface {}

impl Surface {
    /// The first half of Zig's `init`: the runtime fields.
    pub fn new(app: *mut App, opts: &SurfaceOptions) -> Result<Surface, Error> {
        Ok(Surface {
            app,
            platform: Platform::init(opts.platform_tag, opts.platform)?,
            userdata: opts.userdata,
            core_surface: std::ptr::null_mut(),
            content_scale: ContentScale {
                x: opts.scale_factor as f32,
                y: opts.scale_factor as f32,
            },
            size: SurfaceSize {
                width: 800,
                height: 600,
            },
            cursor_pos: CursorPos { x: -1.0, y: -1.0 },
            inspector: None,
            title: None,
        })
    }

    /// Build the configuration for a new surface from the options (the
    /// config half of Zig's `init`).
    pub fn new_surface_config(core_app: &CoreApp, app_config: &Config, opts: &SurfaceOptions) -> Result<Config, Error> {
        // Shallow copy the config so that we can modify it.
        let mut config = apprt::surface::new_config(core_app, app_config, opts.context);

        // If we have a working directory from the options then we set it.
        // SAFETY: C API contract: valid C strings or null.
        if let Some(wd) = unsafe { c_bytes(opts.working_directory) } {
            if !wd.is_empty() {
                let wd = std::string::String::from_utf8_lossy(wd).into_owned();
                'wd: {
                    let stat = match std::fs::metadata(&wd) {
                        Ok(v) => v,
                        Err(err) => {
                            log::warn!(
                                target: LOG,
                                "error opening requested working directory dir={wd} err={err}"
                            );
                            break 'wd;
                        }
                    };

                    if !stat.is_dir() {
                        log::warn!(
                            target: LOG,
                            "requested working directory is not a directory dir={wd}"
                        );
                        break 'wd;
                    }

                    let mut wd_val = crate::config::config::WorkingDirectory::Path(wd);
                    wd_val.finalize();
                    config.working_directory = Some(wd_val);
                }
            }
        }

        // If we have a command from the options then we set it.
        // SAFETY: C API contract.
        if let Some(cmd) = unsafe { c_bytes(opts.command) } {
            if !cmd.is_empty() {
                let cmd = std::string::String::from_utf8_lossy(cmd).into_owned();
                config.command = Some(crate::config::Command::Shell(cmd.into()));
                config.wait_after_command = true;
            }
        }

        // Apply any environment variables that were requested.
        if opts.env_var_count > 0 {
            // SAFETY: C API contract: env_vars has env_var_count entries.
            let env_vars = unsafe { c_slice(opts.env_vars, opts.env_var_count) };
            for env_var in env_vars {
                // SAFETY: C API contract: valid C strings.
                let key = unsafe { CStr::from_ptr(env_var.key) }.to_string_lossy().into_owned();
                let value = unsafe { CStr::from_ptr(env_var.value) }.to_string_lossy().into_owned();
                let map = &mut config.env.map;
                match map.iter_mut().find(|(k, _)| k.as_str() == key) {
                    Some(entry) => entry.1 = value.into(),
                    None => map.push((key.into(), value.into())),
                }
            }
        }

        // If we have an initial input then we set it.
        // SAFETY: C API contract.
        if let Some(input) = unsafe { c_bytes(opts.initial_input) } {
            // We need to escape the string because the "raw" field
            // expects a Zig string.
            let mut buf: Vec<u8> = Vec::new();
            string_escape(input, &mut buf);
            let escaped = std::string::String::from_utf8(buf).expect("escaped strings are ASCII");

            config.input.list.clear();
            config.input.list.push(crate::config::io::ReadableIO::Raw(escaped.into()));
        }

        // Wait after command
        if opts.wait_after_command {
            config.wait_after_command = true;
        }

        Ok(config)
    }

    /// The second half of Zig's `init`: register with the core app and
    /// create the core surface.
    ///
    /// # Safety
    ///
    /// `this` must be a live, heap-allocated surface created by
    /// [`Surface::new`] whose address stays stable until `deinit`.
    #[cfg(all(target_os = "macos", not(test)))]
    pub unsafe fn init(this: *mut Surface, app: *mut App, opts: &SurfaceOptions) -> Result<(), Error> {
        // SAFETY: the caller guarantees `this` and `app` are live.
        let app_ref = unsafe { &*app };
        let core_app = app_ref.core_app;

        // Add ourselves to the list of surfaces on the app.
        // SAFETY: the core app outlives the surface.
        unsafe { (*core_app).add_surface(this) };

        let result = (|| -> Result<(), Error> {
            // SAFETY: the core app is live.
            let config = Self::new_surface_config(unsafe { &*core_app }, &app_ref.config, opts)?;

            // Initialize our surface right away. We're given a view that is
            // ready to use.
            let core = CoreSurface::init(&config, core_app, app, this)?;
            let core = Box::into_raw(core);
            // SAFETY: `this` is live (caller contract).
            unsafe { (*this).core_surface = core };

            // If our options requested a specific font-size, set that.
            if opts.font_size != 0.0 {
                // SAFETY: just created.
                let core_ref = unsafe { &mut *core };
                let mut font_size = core_ref.font_size;
                font_size.points = opts.font_size;
                if let Err(err) = core_ref.set_font_size(font_size) {
                    // SAFETY: created above and not shared yet.
                    unsafe {
                        (*core).deinit();
                        drop(Box::from_raw(core));
                        (*this).core_surface = std::ptr::null_mut();
                    }
                    return Err(err.into());
                }
            }

            Ok(())
        })();

        if result.is_err() {
            // SAFETY: the core app is live.
            unsafe { (*core_app).delete_surface(this) };
        }
        result
    }

    #[cfg(all(target_os = "macos", not(test)))]
    pub fn deinit(&mut self) {
        // Shut down our inspector
        self.free_inspector();

        // Free our title
        self.title = None;

        // Remove ourselves from the list of known surfaces in the app.
        // SAFETY: the app and core app outlive their surfaces.
        unsafe { (*(*self.app).core_app).delete_surface(self) };

        // Clean up our core surface so that all the rendering and IO stop.
        if !self.core_surface.is_null() {
            // SAFETY: created by `init` and freed exactly once, here.
            unsafe {
                (*self.core_surface).deinit();
                drop(Box::from_raw(self.core_surface));
            }
            self.core_surface = std::ptr::null_mut();
        }
    }

    /// Initialize the inspector instance. A surface can only have one
    /// inspector at any given time, so this will return the previous inspector
    /// if it was already initialized.
    pub fn init_inspector(&mut self) -> Result<*mut Inspector, Error> {
        if let Some(v) = &mut self.inspector {
            return Ok(&mut **v);
        }

        let mut inspector = Box::new(Inspector::init(self)?);
        let ptr: *mut Inspector = &mut *inspector;
        self.inspector = Some(inspector);
        Ok(ptr)
    }

    pub fn free_inspector(&mut self) {
        if let Some(mut v) = self.inspector.take() {
            v.deinit();
        }
    }

    pub fn core(&self) -> *mut CoreSurface {
        self.core_surface
    }

    fn core_mut(&self) -> &mut CoreSurface {
        // SAFETY: the core surface is live from `init` until `deinit`, and
        // like Zig it is only used on the main thread.
        unsafe { &mut *self.core_surface }
    }

    fn app(&self) -> &App {
        // SAFETY: the app outlives its surfaces.
        unsafe { &*self.app }
    }

    pub fn rt_app(&self) -> *mut App {
        self.app
    }

    pub fn close(&self, process_alive: bool) {
        let Some(func) = self.app().opts.close_surface else {
            log::info!(target: LOG, "runtime embedder does not support closing a surface");
            return;
        };

        func(self.userdata, process_alive);
    }

    pub fn get_content_scale(&self) -> Result<ContentScale, apprt::Error> {
        Ok(self.content_scale)
    }

    pub fn get_size(&self) -> Result<SurfaceSize, apprt::Error> {
        Ok(self.size)
    }

    pub fn get_title(&self) -> Option<&CStr> {
        self.title.as_deref()
    }

    pub fn supports_clipboard(&self, clipboard_type: Clipboard) -> bool {
        match clipboard_type {
            Clipboard::Standard => true,
            Clipboard::Selection => self.app().opts.supports_selection_clipboard,

            // No embedder can serve the primary selection: the embedding
            // API has no option to declare support for it, and macOS (the
            // only GUI embedder) has no primary selection. The selection
            // flag above covers a distinct custom pasteboard, not this.
            Clipboard::Primary => false,
        }
    }

    pub fn clipboard_request(
        &self,
        clipboard_type: Clipboard,
        state: &ClipboardRequest,
    ) -> Result<ClipboardReadResult, apprt::Error> {
        // Kitty clipboard writes carry their own contents and read
        // nothing from the clipboard, so they skip the read callback
        // entirely: the request is completed immediately, and a
        // completion that requires confirmation diverts into the
        // apprt confirmation flow just like a read.
        if let ClipboardRequest::KittyWrite(_) = state {
            // A write to a location the embedder can't serve is
            // rejected before the transaction completes so the caller
            // answers ENOSYS without a permission prompt, matching
            // reads, where the embedder reports an unsupported
            // location from the read callback. The write callback only
            // fires after any prompt, so it is too late to reject.
            if !self.supports_clipboard(clipboard_type) {
                return Ok(ClipboardReadResult::Unsupported);
            }

            self.complete_kitty_clipboard_write(Box::new(state.clone()));
            return Ok(ClipboardReadResult::Started);
        }

        // We need to allocate to get a pointer to store our clipboard request
        // so that it is stable until the read_clipboard callback and call
        // complete_clipboard_request. This sucks but clipboard requests aren't
        // high throughput so it's probably fine.
        let state_ptr: *mut ClipboardRequest = Box::into_raw(Box::new(state.clone()));
        // SAFETY: just allocated.
        let state_ref = unsafe { &*state_ptr };

        // The representations the read wants served. Text-only
        // requesters ask for the canonical text type; Kitty clipboard
        // reads ask for exactly what the program requested, with
        // text-like aliases normalized so the embedder never has to
        // know about them.
        let text_plain: *const c_char = c"text/plain".as_ptr();
        let mimes: Vec<*const c_char> = match state_ref {
            ClipboardRequest::Paste(_) | ClipboardRequest::Osc52Read(_) => vec![text_plain],

            // A mode 5522 paste event only lists types and must not read
            // any clipboard data.
            ClipboardRequest::List(_) => Vec::new(),

            ClipboardRequest::KittyRead(kitty) => {
                debug_assert!(kitty.mimes.len() <= terminal::kitty::clipboard::MAX_READ_MIMES);
                kitty
                    .mimes
                    .iter()
                    .map(|mime| {
                        if terminal::clipboard::is_text_mime(mime.to_bytes()) {
                            text_plain
                        } else {
                            mime.as_ptr()
                        }
                    })
                    .collect()
            }

            // Handled above.
            ClipboardRequest::KittyWrite(_) => unreachable!(),

            // No clipboard write code paths travel through this function
            ClipboardRequest::Osc52Write(_) => unreachable!(),
        };
        let list = match state_ref {
            // Paste events need the full MIME listing without reading any
            // representation. Kitty reads only ask for it when requested.
            ClipboardRequest::List(_) => true,
            ClipboardRequest::KittyRead(kitty) => kitty.list,
            _ => false,
        };

        let result = (self.app().opts.read_clipboard)(
            self.userdata,
            clipboard_type.backing(),
            state_ptr,
            mimes.as_ptr(),
            mimes.len(),
            list,
        );

        // Only a started request completes later and keeps the state.
        if result != ClipboardReadResult::Started {
            // SAFETY: not handed to the embedder.
            drop(unsafe { Box::from_raw(state_ptr) });
        }
        Ok(result)
    }

    /// Complete a Kitty clipboard protocol write request. The apprt
    /// contributes nothing to the completion (the authoritative
    /// contents live in the request itself), but a completion that
    /// requires confirmation diverts into the confirm callback, which
    /// keeps the state alive for the asynchronous prompt.
    fn complete_kitty_clipboard_write(&self, state: Box<ClipboardRequest>) {
        let ClipboardRequest::KittyWrite(kitty) = &*state else {
            unreachable!("kitty write state");
        };

        match self.core_mut().complete_clipboard_request(&state, CompleteClipboard::default()) {
            Ok(()) => {}
            Err(core_surface::Error::UnauthorizedPaste) => {
                // Convert the would-be contents so the permission
                // prompt can display exactly what would be written.
                let contents: Vec<capi::ClipboardContent> = kitty
                    .contents
                    .iter()
                    .map(|src| capi::ClipboardContent {
                        mime: src.mime.as_ptr(),
                        data: src.data.as_ptr(),
                        len: src.data.len(),
                    })
                    .collect();

                let confirm = capi::ClipboardConfirm {
                    contents: contents.as_ptr(),
                    contents_len: contents.len(),
                    available: std::ptr::null(),
                    available_len: 0,
                    name: if !kitty.name.as_bytes().is_empty() {
                        kitty.name.as_ptr()
                    } else {
                        std::ptr::null()
                    },
                    can_remember: !kitty.pw.is_empty(),
                };
                let tag = state.tag() as c_int;
                let state_ptr = Box::into_raw(state);
                (self.app().opts.confirm_read_clipboard)(self.userdata, &confirm, state_ptr, tag);
                return;
            }
            Err(err) => log::error!(target: LOG, "error completing clipboard request err={err}"),
        }

        // We don't defer this because the clipboard confirmation route
        // preserves the clipboard request.
        drop(state);
    }

    fn complete_clipboard_request(&self, complete: &capi::ClipboardComplete, state: Box<ClipboardRequest>) {
        // Convert the C representations to the core types. Everything
        // remains borrowed from the caller for the duration of the call.
        // SAFETY: C API contract: the arrays have the given lengths.
        let raw_contents = unsafe { c_slice(complete.contents, complete.contents_len) };
        let contents: Vec<terminal::clipboard::Content<'_>> = raw_contents
            .iter()
            .map(|raw| terminal::clipboard::Content {
                // SAFETY: C API contract: valid strings and data.
                mime: unsafe { CStr::from_ptr(raw.mime) }.to_bytes(),
                data: unsafe { c_slice(raw.data, raw.len) },
            })
            .collect();

        // SAFETY: C API contract.
        let raw_available = unsafe { c_slice(complete.available, complete.available_len) };
        let available: Vec<&[u8]> = raw_available
            .iter()
            // SAFETY: C API contract: valid strings.
            .map(|&raw| unsafe { CStr::from_ptr(raw) }.to_bytes())
            .collect();

        // Attempt to complete the request, but we may request
        // confirmation.
        match self.core_mut().complete_clipboard_request(
            &state,
            CompleteClipboard {
                contents: &contents,
                available: &available,
                confirmed: complete.confirmed,
                remember: complete.remember,
            },
        ) {
            Ok(()) => {}
            Err(core_surface::Error::UnsafePaste | core_surface::Error::UnauthorizedPaste) => {
                // Session grant information for the permission prompt,
                // carried only by Kitty clipboard protocol requests.
                let (name, can_remember): (*const c_char, bool) = match &*state {
                    ClipboardRequest::KittyRead(kitty) => (
                        if !kitty.name.as_bytes().is_empty() {
                            kitty.name.as_ptr()
                        } else {
                            std::ptr::null()
                        },
                        !kitty.pw.is_empty(),
                    ),
                    ClipboardRequest::KittyWrite(kitty) => (
                        if !kitty.name.as_bytes().is_empty() {
                            kitty.name.as_ptr()
                        } else {
                            std::ptr::null()
                        },
                        !kitty.pw.is_empty(),
                    ),
                    _ => (std::ptr::null(), false),
                };

                let confirm = capi::ClipboardConfirm {
                    contents: complete.contents,
                    contents_len: complete.contents_len,
                    available: complete.available,
                    available_len: complete.available_len,
                    name,
                    can_remember,
                };
                let tag = state.tag() as c_int;
                let state_ptr = Box::into_raw(state);
                (self.app().opts.confirm_read_clipboard)(self.userdata, &confirm, state_ptr, tag);
                return;
            }
            Err(err) => log::error!(target: LOG, "error completing clipboard request err={err}"),
        }

        // We don't defer this because the clipboard confirmation route
        // preserves the clipboard request.
        drop(state);
    }

    fn deny_clipboard_request(&self, state: Box<ClipboardRequest>) {
        self.core_mut().deny_clipboard_request(&state);
        drop(state);
    }

    pub fn set_clipboard(
        &self,
        clipboard_type: Clipboard,
        contents: &[ClipboardContent<'_>],
        confirm: bool,
    ) -> Result<(), apprt::Error> {
        // The C API needs NUL-terminated MIME types.
        let mimes: Vec<CString> = contents
            .iter()
            .map(|content| CString::new(content.mime).map_err(|e| apprt::Error(e.to_string())))
            .collect::<Result<_, _>>()?;
        let array: Vec<capi::ClipboardContent> = contents
            .iter()
            .zip(&mimes)
            .map(|(content, mime)| capi::ClipboardContent {
                mime: mime.as_ptr(),
                data: content.data.as_ptr(),
                len: content.data.len(),
            })
            .collect();

        (self.app().opts.write_clipboard)(
            self.userdata,
            clipboard_type.backing(),
            array.as_ptr(),
            array.len(),
            confirm,
        );
        Ok(())
    }

    pub fn get_cursor_pos(&self) -> Result<CursorPos, apprt::Error> {
        Ok(self.cursor_pos)
    }

    pub fn refresh(&self) {
        if let Err(err) = self.core_mut().refresh_callback() {
            log::error!(target: LOG, "error in refresh callback err={err}");
        }
    }

    pub fn draw(&self) {
        if let Err(err) = self.core_mut().draw() {
            log::error!(target: LOG, "error in draw err={err}");
        }
    }

    pub fn update_content_scale(&mut self, x: f64, y: f64) {
        // We are an embedded API so the caller can send us all sorts of
        // garbage. We want to make sure that the float values are valid
        // and we don't want to support fractional scaling below 1.
        let x_scaled = f64::max(1.0, if x.is_nan() { 1.0 } else { x });
        let y_scaled = f64::max(1.0, if y.is_nan() { 1.0 } else { y });

        self.content_scale = ContentScale {
            x: x_scaled as f32,
            y: y_scaled as f32,
        };

        if let Err(err) = self.core_mut().content_scale_callback(self.content_scale) {
            log::error!(target: LOG, "error in content scale callback err={err}");
        }
    }

    pub fn update_size(&mut self, width: u32, height: u32) {
        // Runtimes sometimes generate superfluous resize events even
        // if the size did not actually change (SwiftUI). We check
        // that the size actually changed from what we last recorded
        // since resizes are expensive.
        if self.size.width == width && self.size.height == height {
            return;
        }

        self.size = SurfaceSize { width, height };

        // Call the primary callback.
        if let Err(err) = self.core_mut().size_callback(self.size) {
            log::error!(target: LOG, "error in size callback err={err}");
        }
    }

    pub fn color_scheme_callback(&self, scheme: ColorScheme) {
        if let Err(err) = self.core_mut().color_scheme_callback(scheme) {
            log::error!(target: LOG, "error setting color scheme err={err}");
        }
    }

    pub fn mouse_button_callback(
        &self,
        action: input::MouseButtonState,
        button: input::MouseButton,
        mods: input::Mods,
    ) -> bool {
        match self.core_mut().mouse_button_callback(action, button, mods) {
            Ok(v) => v,
            Err(err) => {
                log::error!(target: LOG, "error in mouse button callback err={err}");
                false
            }
        }
    }

    pub fn mouse_pressure_callback(&self, stage: input::MousePressureStage, pressure: f64) {
        if let Err(err) = self.core_mut().mouse_pressure_callback(stage, pressure) {
            log::error!(target: LOG, "error in mouse pressure callback err={err}");
        }
    }

    pub fn scroll_callback(&self, xoff: f64, yoff: f64, mods: input::ScrollMods) {
        if let Err(err) = self.core_mut().scroll_callback(xoff, yoff, mods) {
            log::error!(target: LOG, "error in scroll callback err={err}");
        }
    }

    pub fn cursor_pos_callback(&mut self, x: f64, y: f64, mods: input::Mods) {
        // Convert our unscaled x/y to scaled.
        let pos = match self.cursor_pos_to_pixels(CursorPos {
            x: x as f32,
            y: y as f32,
        }) {
            Ok(v) => v,
            Err(err) => {
                log::error!(
                    target: LOG,
                    "error converting cursor pos to scaled pixels in cursor pos callback err={err}"
                );
                return;
            }
        };

        // There are cases where the platform reports a mouse motion event
        // without the cursor actually moving. For example, on macOS, updating
        // the window title can trigger a phantom mouse-move event at the same
        // coordinates. This can cause the mouse to incorrectly unhide when
        // mouse-hide-while-typing is enabled (commonly seen with TUI apps
        // like Zellij that frequently update the title). To prevent incorrect
        // behavior, we only continue with callback logic if the cursor has
        // actually moved.
        if (self.cursor_pos.x - pos.x).abs() < 1.0 && (self.cursor_pos.y - pos.y).abs() < 1.0 {
            return;
        }

        self.cursor_pos = pos;

        if let Err(err) = self.core_mut().cursor_pos_callback(self.cursor_pos, Some(mods)) {
            log::error!(target: LOG, "error in cursor pos callback err={err}");
        }
    }

    pub fn preedit_callback(&self, preedit_: Option<&[u8]>) {
        if let Err(err) = self.core_mut().preedit_callback(preedit_) {
            log::error!(target: LOG, "error in preedit callback err={err}");
        }
    }

    pub fn text_callback(&self, text: &[u8]) {
        if let Err(err) = self.core_mut().text_callback(text) {
            log::error!(target: LOG, "error in key callback err={err}");
        }
    }

    /// Paste explicit text into the surface as if it came from the
    /// clipboard, regardless of the actual clipboard contents. Unlike
    /// textCallback, this honors paste protection: unsafe text routes
    /// through the confirm_read_clipboard callback before it is sent.
    ///
    /// `text` must be followed by a NUL byte (Zig: `[:0]const u8`).
    pub fn paste_text(&self, text: &[u8]) {
        if text.is_empty() {
            return;
        }

        // The request state must be heap allocated because it outlives
        // this call if confirmation is required. completeClipboardRequest
        // takes ownership of it in every path.
        let state = Box::new(ClipboardRequest::Paste(Clipboard::Standard));

        let contents = [capi::ClipboardContent {
            mime: c"text/plain".as_ptr(),
            data: text.as_ptr(),
            len: text.len(),
        }];
        self.complete_clipboard_request(
            &capi::ClipboardComplete {
                contents: contents.as_ptr(),
                contents_len: contents.len(),
                available: std::ptr::null(),
                available_len: 0,
                confirmed: false,
                remember: false,
            },
            state,
        );
    }

    pub fn focus_callback(&self, focused: bool) {
        if let Err(err) = self.core_mut().focus_callback(focused) {
            log::error!(target: LOG, "error in focus callback err={err}");
        }
    }

    pub fn occlusion_callback(&self, visible: bool) {
        if let Err(err) = self.core_mut().occlusion_callback(visible) {
            log::error!(target: LOG, "error in occlusion callback err={err}");
        }
    }

    fn queue_inspector_render(&self) {
        if let Err(err) = self
            .app()
            .perform_action(Target::Surface(self.core_surface), Action::RenderInspector)
        {
            log::error!(target: LOG, "error rendering the inspector err={err}");
        }
    }

    pub fn new_surface_options(&self, context: NewSurfaceContext) -> SurfaceOptions {
        let font_size: f32 = if !self.app().config.window_inherit_font_size {
            0.0
        } else {
            self.core_mut().font_size.points
        };

        let working_directory: *const c_char = 'wd: {
            if !apprt::surface::should_inherit_working_directory(context, &self.app().config) {
                break 'wd std::ptr::null();
            }
            let Some(cwd) = self.core_mut().pwd() else {
                break 'wd std::ptr::null();
            };
            // Ownership passes to the embedder (Zig: an unfreed dupeZ).
            match CString::new(cwd) {
                Ok(v) => v.into_raw(),
                Err(_) => std::ptr::null(),
            }
        };

        SurfaceOptions {
            font_size,
            working_directory,
            context,
            ..SurfaceOptions::default()
        }
    }

    pub fn default_termio_env(&self) -> Result<EnvMap, apprt::Error> {
        #[allow(unused_mut)]
        let mut env = crate::global::environ_map();

        #[cfg(target_os = "macos")]
        {
            if env.contains_key("__XCODE_BUILT_PRODUCTS_DIR_PATHS") {
                env.remove("__XCODE_BUILT_PRODUCTS_DIR_PATHS");
                env.remove("__XPC_DYLD_LIBRARY_PATH");
                env.remove("DYLD_FRAMEWORK_PATH");
                env.remove("DYLD_INSERT_LIBRARIES");
                env.remove("DYLD_LIBRARY_PATH");
                env.remove("LD_LIBRARY_PATH");
                env.remove("SECURITYSESSIONID");
                env.remove("XPC_SERVICE_NAME");
            }

            // Remove this so that running `ghostty` within Ghostty works.
            env.remove("GHOSTTY_MAC_LAUNCH_SOURCE");

            // If we were launched from the desktop then we want to
            // remove the LANGUAGE env var so that we don't inherit
            // our translation settings for Ghostty. If we aren't from
            // the desktop then we didn't set our LANGUAGE var so we
            // don't need to remove it.
            if crate::os::launched_from_desktop() {
                env.remove("LANGUAGE");
            }
        }

        Ok(env)
    }

    /// The cursor position from the host directly is in screen coordinates but
    /// all our interface works in pixels.
    fn cursor_pos_to_pixels(&self, pos: CursorPos) -> Result<CursorPos, apprt::Error> {
        let scale = self.get_content_scale()?;
        Ok(CursorPos {
            x: pos.x * scale.x,
            y: pos.y * scale.y,
        })
    }

    pub fn render_device_options(&self) -> Result<renderer::DeviceOptions, apprt::Error> {
        // The Metal renderer currently does not have any options
        Ok(renderer::DeviceOptions::default())
    }
}

/// `Inspector.Backend`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectorBackend {
    Metal,
}

impl InspectorBackend {
    pub fn deinit(self) {
        match self {
            InspectorBackend::Metal => {
                #[cfg(all(target_os = "macos", feature = "inspector"))]
                // SAFETY: the Metal backend was initialized on the current
                // ImGui context.
                unsafe {
                    crate::pkg::dcimgui::ImGui_ImplMetal_Shutdown()
                };
            }
        }
    }
}

/// Inspector is the state required for the terminal inspector. A terminal
/// inspector is 1:1 with a Surface.
#[derive(Debug)]
pub struct Inspector {
    pub surface: *mut Surface,
    #[cfg(feature = "inspector")]
    pub ig_ctx: *mut crate::pkg::dcimgui::c::ImGuiContext,
    pub backend: Option<InspectorBackend>,
    pub content_scale: f64,

    /// Our previous instant used to calculate delta time for animations.
    pub instant: Option<std::time::Instant>,
}

#[cfg(feature = "inspector")]
use crate::pkg::dcimgui::{self as cimgui, c as imgui_c};

impl Inspector {
    #[cfg(feature = "inspector")]
    pub fn init(surface: &mut Surface) -> Result<Inspector, Error> {
        // SAFETY: plain ImGui context management.
        let ig_ctx = unsafe { imgui_c::ImGui_CreateContext(std::ptr::null_mut()) };
        if ig_ctx.is_null() {
            return Err(Error::Inspector);
        }
        // SAFETY: the context was just created.
        unsafe {
            imgui_c::ImGui_SetCurrentContext(ig_ctx);
            let io = imgui_c::ImGui_GetIO();
            (*io).BackendPlatformName = c"ghostty_embedded".as_ptr();
        }

        // Setup our core inspector
        crate::inspector::Inspector::setup();
        if let Err(err) = surface.core_mut().activate_inspector() {
            log::error!(target: LOG, "failed to activate inspector err={err}");
        }

        Ok(Inspector {
            surface,
            ig_ctx,
            backend: None,
            content_scale: 1.0,
            instant: None,
        })
    }

    /// Without Dear ImGui (the `inspector` feature) there is no inspector.
    #[cfg(not(feature = "inspector"))]
    pub fn init(surface: &mut Surface) -> Result<Inspector, Error> {
        let _ = surface;
        Err(Error::Inspector)
    }

    fn surface(&self) -> &Surface {
        // SAFETY: the inspector is owned by (and outlived by) its surface.
        unsafe { &*self.surface }
    }

    /// Make our ImGui context current and return its IO.
    #[cfg(feature = "inspector")]
    fn io(&self) -> *mut imgui_c::ImGuiIO {
        // SAFETY: our context is live until deinit.
        unsafe {
            imgui_c::ImGui_SetCurrentContext(self.ig_ctx);
            imgui_c::ImGui_GetIO()
        }
    }

    pub fn deinit(&mut self) {
        self.surface().core_mut().deactivate_inspector();
        #[cfg(feature = "inspector")]
        // SAFETY: our context is live until here.
        unsafe {
            imgui_c::ImGui_SetCurrentContext(self.ig_ctx);
            if let Some(v) = self.backend {
                v.deinit();
            }
            imgui_c::ImGui_DestroyContext(self.ig_ctx);
        }
    }

    /// Queue a render for the next frame.
    pub fn queue_render(&self) {
        self.surface().queue_inspector_render();
    }

    /// Initialize the inspector for a metal backend.
    #[cfg(target_os = "macos")]
    pub fn init_metal(&mut self, device: objc::Object) -> bool {
        #[cfg(feature = "inspector")]
        {
            // SAFETY: our context is live; device is a valid MTLDevice.
            unsafe { imgui_c::ImGui_SetCurrentContext(self.ig_ctx) };

            if let Some(v) = self.backend.take() {
                v.deinit();
            }

            // SAFETY: valid MTLDevice.
            let ok = unsafe { cimgui::ImGui_ImplMetal_Init(device.value) };
            device.release();
            if !ok {
                log::warn!(target: LOG, "failed to initialize metal backend");
                return false;
            }
            self.backend = Some(InspectorBackend::Metal);

            log::debug!(target: LOG, "initialized metal backend");
            true
        }

        #[cfg(not(feature = "inspector"))]
        {
            device.release();
            false
        }
    }

    #[cfg(target_os = "macos")]
    pub fn render_metal(&mut self, command_buffer: objc::Object, desc: objc::Object) -> Result<(), Error> {
        #[cfg(feature = "inspector")]
        {
            debug_assert!(self.backend == Some(InspectorBackend::Metal));
            //log::debug!(target: LOG, "render");

            // Setup our imgui frame. We need to render multiple frames to ensure
            // ImGui completes all its state processing. I don't know how to fix
            // this.
            for _ in 0..2 {
                // SAFETY: valid render pass descriptor; our context is current.
                unsafe { cimgui::ImGui_ImplMetal_NewFrame(desc.value) };
                self.new_frame();
                // SAFETY: our context is current.
                unsafe { imgui_c::ImGui_NewFrame() };

                // Build our UI
                let core = self.surface().core_surface;
                // SAFETY: the core surface is live; the context is current.
                unsafe {
                    if let Some(inspector) = (*core).inspector.as_mut() {
                        let inspector: *mut crate::inspector::Inspector = &mut **inspector;
                        (*inspector).render(core);
                    }
                }

                // Render
                // SAFETY: our context is current.
                unsafe { imgui_c::ImGui_Render() };
            }

            // MTLRenderCommandEncoder
            // SAFETY: valid command buffer and descriptor.
            unsafe {
                let encoder: objc::Object = command_buffer.msg_send(
                    objc::sel(c"renderCommandEncoderWithDescriptor:"),
                    (desc.value,),
                );
                cimgui::ImGui_ImplMetal_RenderDrawData(
                    imgui_c::ImGui_GetDrawData(),
                    command_buffer.value,
                    encoder.value,
                );
                encoder.msg_send::<(), _>(objc::sel(c"endEncoding"), ());
            }
        }

        command_buffer.release();
        desc.release();
        Ok(())
    }

    pub fn update_content_scale(&mut self, x: f64, _y: f64) {
        // Cache our scale because we use it for cursor position calculations.
        self.content_scale = x;

        #[cfg(feature = "inspector")]
        // SAFETY: our context is live.
        unsafe {
            imgui_c::ImGui_SetCurrentContext(self.ig_ctx);

            // Setup a new style and scale it appropriately. We must use the
            // ImGuiStyle constructor to get proper default values (e.g.,
            // CurveTessellationTol) rather than zero-initialized values.
            let mut style: imgui_c::ImGuiStyle = std::mem::zeroed();
            cimgui::ext::ImGuiStyle_ImGuiStyle(&mut style);
            imgui_c::ImGuiStyle_ScaleAllSizes(&mut style, x as f32);
            let active_style = imgui_c::ImGui_GetStyle();
            *active_style = style;
        }
    }

    pub fn update_size(&mut self, width: u32, height: u32) {
        #[cfg(feature = "inspector")]
        // SAFETY: valid IO of our context.
        unsafe {
            (*self.io()).DisplaySize = imgui_c::ImVec2 {
                x: width as f32,
                y: height as f32,
            };
        }
        #[cfg(not(feature = "inspector"))]
        let _ = (width, height);
    }

    pub fn mouse_button_callback(
        &mut self,
        action: input::MouseButtonState,
        button: input::MouseButton,
        _mods: input::Mods,
    ) {
        self.queue_render();

        #[cfg(feature = "inspector")]
        {
            let io = self.io();

            let imgui_button = match button {
                input::MouseButton::Left => imgui_c::ImGuiMouseButton_Left,
                input::MouseButton::Middle => imgui_c::ImGuiMouseButton_Middle,
                input::MouseButton::Right => imgui_c::ImGuiMouseButton_Right,
                _ => return, // unsupported
            };

            // SAFETY: valid IO of our context.
            unsafe {
                imgui_c::ImGuiIO_AddMouseButtonEvent(
                    io,
                    imgui_button as c_int,
                    action == input::MouseButtonState::Press,
                )
            };
        }
        #[cfg(not(feature = "inspector"))]
        let _ = (action, button);
    }

    pub fn scroll_callback(&mut self, xoff: f64, yoff: f64, mods: input::ScrollMods) {
        self.queue_render();

        // For precision scrolling (trackpads), the values are in pixels which
        // scroll way too fast. Scale them down to approximate discrete wheel
        // notches. imgui expects 1.0 to scroll ~5 lines of text.
        let scale: f64 = if mods.precision { 0.1 } else { 1.0 };

        #[cfg(feature = "inspector")]
        // SAFETY: valid IO of our context.
        unsafe {
            imgui_c::ImGuiIO_AddMouseWheelEvent(self.io(), (xoff * scale) as f32, (yoff * scale) as f32)
        };
        #[cfg(not(feature = "inspector"))]
        let _ = (xoff, yoff, scale);
    }

    pub fn cursor_pos_callback(&mut self, x: f64, y: f64) {
        self.queue_render();

        #[cfg(feature = "inspector")]
        // SAFETY: valid IO of our context.
        unsafe {
            imgui_c::ImGuiIO_AddMousePosEvent(
                self.io(),
                (x * self.content_scale) as f32,
                (y * self.content_scale) as f32,
            )
        };
        #[cfg(not(feature = "inspector"))]
        let _ = (x, y);
    }

    pub fn focus_callback(&mut self, focused: bool) {
        self.queue_render();

        #[cfg(feature = "inspector")]
        // SAFETY: valid IO of our context.
        unsafe {
            imgui_c::ImGuiIO_AddFocusEvent(self.io(), focused)
        };
        #[cfg(not(feature = "inspector"))]
        let _ = focused;
    }

    pub fn text_callback(&mut self, text: &CStr) {
        self.queue_render();

        #[cfg(feature = "inspector")]
        // SAFETY: valid IO of our context; text is NUL-terminated.
        unsafe {
            imgui_c::ImGuiIO_AddInputCharactersUTF8(self.io(), text.as_ptr())
        };
        #[cfg(not(feature = "inspector"))]
        let _ = text;
    }

    pub fn key_callback(&mut self, action: input::Action, key: input::Key, mods: input::Mods) -> Result<(), Error> {
        self.queue_render();

        #[cfg(feature = "inspector")]
        // SAFETY: valid IO of our context.
        unsafe {
            let io = self.io();

            // Update all our modifiers
            imgui_c::ImGuiIO_AddKeyEvent(io, imgui_c::ImGuiKey_LeftShift as imgui_c::ImGuiKey, mods.shift);
            imgui_c::ImGuiIO_AddKeyEvent(io, imgui_c::ImGuiKey_LeftCtrl as imgui_c::ImGuiKey, mods.ctrl);
            imgui_c::ImGuiIO_AddKeyEvent(io, imgui_c::ImGuiKey_LeftAlt as imgui_c::ImGuiKey, mods.alt);
            imgui_c::ImGuiIO_AddKeyEvent(io, imgui_c::ImGuiKey_LeftSuper as imgui_c::ImGuiKey, mods.super_);

            // Send our keypress
            if let Some(imgui_key) = key.imgui_key() {
                imgui_c::ImGuiIO_AddKeyEvent(
                    io,
                    imgui_key,
                    action == input::Action::Press || action == input::Action::Repeat,
                );
            }
        }
        #[cfg(not(feature = "inspector"))]
        let _ = (action, key, mods);

        Ok(())
    }

    #[cfg(feature = "inspector")]
    fn new_frame(&mut self) {
        // SAFETY: our context is current.
        let io = unsafe { imgui_c::ImGui_GetIO() };

        // Determine our delta time
        let now = std::time::Instant::now();
        let delta = match self.instant {
            Some(prev) => {
                let since_s = now.duration_since(prev).as_secs_f64() as f32;
                f32::max(0.00001, since_s)
            }
            None => 1.0 / 60.0,
        };
        // SAFETY: valid IO.
        unsafe { (*io).DeltaTime = delta };
        self.instant = Some(now);
    }
}

/// C API (Zig: `CAPI`).
pub mod capi {
    use super::*;

    /// This is the same as Surface.KeyEvent but this is the raw C API version.
    /// (ghostty_input_key_s)
    #[repr(C)]
    #[derive(Debug, Clone, Copy)]
    pub struct KeyEvent {
        pub action: input::Action,
        pub mods: c_int,
        pub consumed_mods: c_int,
        pub keycode: u32,
        pub text: *const c_char,
        pub unshifted_codepoint: u32,
        pub composing: bool,
    }

    /// Zig: `@bitCast(@as(Mods.Backing, @truncate(@as(c_uint, @bitCast(v)))))`.
    pub fn mods_from_c(v: c_int) -> input::Mods {
        input::Mods::from_int(v as c_uint as input::key_mods::ModsBacking)
    }

    /// Zig: `@bitCast(@as(u8, @truncate(@as(c_uint, @bitCast(v)))))`.
    pub fn scroll_mods_from_c(v: c_int) -> input::ScrollMods {
        input::ScrollMods::from_bits(v as c_uint as u8)
    }

    impl KeyEvent {
        /// Convert to Zig key event.
        ///
        /// # Safety
        ///
        /// `text` must be null or a valid C string that outlives `'a`.
        pub unsafe fn key_event<'a>(&self) -> super::KeyEvent<'a> {
            super::KeyEvent {
                action: self.action,
                mods: mods_from_c(self.mods),
                consumed_mods: mods_from_c(self.consumed_mods),
                keycode: self.keycode,
                // SAFETY: forwarded from the caller.
                text: unsafe { c_bytes(self.text) },
                unshifted_codepoint: self.unshifted_codepoint,
                composing: self.composing,
            }
        }
    }

    /// ghostty_surface_size_s
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct SurfaceSize {
        pub columns: u16,
        pub rows: u16,
        pub width_px: u32,
        pub height_px: u32,
        pub cell_width_px: u32,
        pub cell_height_px: u32,
    }

    /// ghostty_clipboard_content_s
    ///
    /// One representation of clipboard contents. The data is binary-safe
    /// and its length is explicit; it is not sentinel-terminated.
    #[repr(C)]
    #[derive(Debug, Clone, Copy)]
    pub struct ClipboardContent {
        pub mime: *const c_char,
        pub data: *const u8,
        pub len: usize,
    }

    /// ghostty_clipboard_complete_s
    ///
    /// The payload for completing a clipboard read request. See
    /// Surface.CompleteClipboard for the field documentation.
    #[repr(C)]
    #[derive(Debug, Clone, Copy)]
    pub struct ClipboardComplete {
        pub contents: *const ClipboardContent,
        pub contents_len: usize,
        pub available: *const *const c_char,
        pub available_len: usize,
        pub confirmed: bool,
        pub remember: bool,
    }

    /// ghostty_clipboard_confirm_s
    ///
    /// The payload of a clipboard read confirmation request: the
    /// would-be completion contents plus the information shown in the
    /// permission prompt. All memory is borrowed for the duration of
    /// the confirm_read_clipboard callback.
    #[repr(C)]
    #[derive(Debug, Clone, Copy)]
    pub struct ClipboardConfirm {
        pub contents: *const ClipboardContent,
        pub contents_len: usize,
        pub available: *const *const c_char,
        pub available_len: usize,

        /// The human friendly name of the requesting program for the
        /// prompt, null when the protocol doesn't carry one.
        pub name: *const c_char,

        /// True when the user's decision may be remembered as a
        /// session grant, reported back through the completion's
        /// remember field.
        pub can_remember: bool,
    }

    /// ghostty_text_s
    #[repr(C)]
    #[derive(Debug, Clone, Copy)]
    pub struct Text {
        pub tl_px_x: f64,
        pub tl_px_y: f64,
        pub offset_start: u32,
        pub offset_len: u32,
        pub text: *const c_char,
        pub text_len: usize,
    }

    impl Text {
        /// Free the text.
        ///
        /// # Safety
        ///
        /// The text must have been filled in by this API and not freed.
        pub unsafe fn deinit(&mut self) {
            if !self.text.is_null() {
                // SAFETY: allocated by `read_text_locked` as a boxed slice
                // of text_len + 1 bytes (including the terminator).
                unsafe {
                    drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                        self.text as *mut u8,
                        self.text_len + 1,
                    )));
                }
                self.text = std::ptr::null();
            }
        }
    }

    /// ghostty_point_tag_e (`Point.Tag`)
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum PointTag {
        Active = 0,
        Viewport = 1,
        Screen = 2,
        History = 3,
    }

    /// ghostty_point_coord_e (`Point.CoordTag`)
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum PointCoordTag {
        Exact = 0,
        TopLeft = 1,
        BottomRight = 2,
    }

    /// ghostty_point_s
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Point {
        pub tag: PointTag,
        pub coord_tag: PointCoordTag,
        pub x: u32,
        pub y: u32,
    }

    impl Point {
        pub fn pin(&self, screen: &terminal::Screen) -> Option<terminal::Pin> {
            use terminal::point::{self, Coordinate};
            use terminal::size::CellCountInt;

            // The core point tag.
            let tag: point::Tag = match self.tag {
                PointTag::Active => point::Tag::Active,
                PointTag::Viewport => point::Tag::Viewport,
                PointTag::Screen => point::Tag::Screen,
                PointTag::History => point::Tag::History,
            };

            // Clamp our point to the screen bounds.
            let clamped_x = self.x.min((screen.pages.cols as u32).saturating_sub(1));
            let clamped_y = self.y.min((screen.pages.rows as u32).saturating_sub(1));

            match self.coord_tag {
                // Exact coordinates require a specific pin.
                PointCoordTag::Exact => {
                    let pt_x = CellCountInt::try_from(clamped_x).unwrap_or(CellCountInt::MAX);
                    let coord = Coordinate {
                        x: pt_x,
                        y: clamped_y,
                    };

                    let pt = match tag {
                        point::Tag::Active => point::Point::Active(coord),
                        point::Tag::Viewport => point::Point::Viewport(coord),
                        point::Tag::Screen => point::Point::Screen(coord),
                        point::Tag::History => point::Point::History(coord),
                    };

                    screen.pages.pin(pt)
                }

                PointCoordTag::TopLeft => Some(screen.pages.get_top_left(tag)),

                PointCoordTag::BottomRight => screen.pages.get_bottom_right(tag),
            }
        }
    }

    /// ghostty_selection_s
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Selection {
        pub tl: Point,
        pub br: Point,
        pub rectangle: bool,
    }

    impl Selection {
        pub fn core(&self, screen: &terminal::Screen) -> Option<terminal::Selection> {
            Some(terminal::Selection::init(
                self.tl.pin(screen)?,
                self.br.pin(screen)?,
                self.rectangle,
            ))
        }
    }

    /// Dereference a C API surface handle.
    ///
    /// # Safety
    ///
    /// `ptr` must be a live surface returned by `ghostty_surface_new`.
    unsafe fn surface<'a>(ptr: *mut Surface) -> &'a mut Surface {
        // SAFETY: forwarded from the caller.
        unsafe { &mut *ptr }
    }

    /// Dereference a C API app handle.
    ///
    /// # Safety
    ///
    /// `ptr` must be a live app returned by `ghostty_app_new`.
    unsafe fn app<'a>(ptr: *mut App) -> &'a mut App {
        // SAFETY: forwarded from the caller.
        unsafe { &mut *ptr }
    }

    /// Dereference a C API inspector handle.
    ///
    /// # Safety
    ///
    /// `ptr` must be a live inspector returned by `ghostty_surface_inspector`.
    unsafe fn inspector<'a>(ptr: *mut Inspector) -> &'a mut Inspector {
        // SAFETY: forwarded from the caller.
        unsafe { &mut *ptr }
    }

    /// Create a new app.
    ///
    /// # Safety
    ///
    /// `opts` and `config` must be valid.
    #[cfg(all(target_os = "macos", not(test)))]
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_app_new(opts: *const Options, config: *const Config) -> *mut App {
        // SAFETY: forwarded from the caller.
        match unsafe { app_new_(&*opts, &*config) } {
            Ok(v) => v,
            Err(err) => {
                log::error!(target: LOG, "error initializing app err={err}");
                std::ptr::null_mut()
            }
        }
    }

    #[cfg(all(target_os = "macos", not(test)))]
    fn app_new_(opts: &Options, config: &Config) -> Result<*mut App, Error> {
        let core_app = Box::into_raw(CoreApp::create().map_err(Error::CreateApp)?);

        // Create our runtime app
        match App::init(core_app, config, *opts) {
            Ok(app) => Ok(Box::into_raw(Box::new(app))),
            Err(err) => {
                // SAFETY: created above and not shared.
                unsafe { Box::from_raw(core_app) }.destroy();
                Err(err)
            }
        }
    }

    /// Tick the event loop. This should be called whenever the "wakeup"
    /// callback is invoked for the runtime.
    ///
    /// # Safety
    ///
    /// `v` must be a live app.
    #[cfg(all(target_os = "macos", not(test)))]
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_app_tick(v: *mut App) {
        // SAFETY: forwarded from the caller.
        let v = unsafe { app(v) };
        if let Err(err) = v.core_app().tick(v) {
            log::error!(target: LOG, "error app tick err={err}");
        }
    }

    /// Return the userdata associated with the app.
    ///
    /// # Safety
    ///
    /// `v` must be a live app.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_app_userdata(v: *mut App) -> *mut c_void {
        // SAFETY: forwarded from the caller.
        unsafe { app(v) }.opts.userdata
    }

    /// # Safety
    ///
    /// `v` must be a live app; it is freed.
    #[cfg(all(target_os = "macos", not(test)))]
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_app_free(v: *mut App) {
        // SAFETY: forwarded from the caller.
        let mut v = unsafe { Box::from_raw(v) };
        let core_app = v.core_app;
        v.terminate();
        drop(v);
        // SAFETY: created by ghostty_app_new.
        unsafe { Box::from_raw(core_app) }.destroy();
    }

    /// Update the focused state of the app.
    ///
    /// # Safety
    ///
    /// `v` must be a live app.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_app_set_focus(v: *mut App, focused: bool) {
        // SAFETY: forwarded from the caller.
        unsafe { app(v) }.focus_event(focused);
    }

    /// Notify the app of a global keypress capture. This will return
    /// true if the key was captured by the app, in which case the caller
    /// should not process the key.
    ///
    /// # Safety
    ///
    /// `v` must be a live app and the event text a valid C string or null.
    #[cfg(all(target_os = "macos", not(test)))]
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_app_key(v: *mut App, event: KeyEvent) -> bool {
        // SAFETY: forwarded from the caller.
        match unsafe { app(v).key_event(KeyTarget::App, event.key_event()) } {
            Ok(v) => v,
            Err(err) => {
                log::warn!(target: LOG, "error processing key event err={err}");
                false
            }
        }
    }

    /// Returns true if the given key event would trigger a binding
    /// if it were sent to the surface right now. The "right now"
    /// is important because things like trigger sequences are only
    /// valid until the next key event.
    ///
    /// # Safety
    ///
    /// `config` must be a live config and the event text a valid C string
    /// or null.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_config_key_is_binding(config: *mut Config, event: KeyEvent) -> bool {
        // SAFETY: forwarded from the caller.
        let Some(core_event) = (unsafe { event.key_event() }).core() else {
            log::warn!(target: LOG, "error processing key event");
            return false;
        };

        // SAFETY: forwarded from the caller.
        unsafe { &*config }.key_event_is_binding(&core_event)
    }

    /// Notify the app that the keyboard was changed. This causes the
    /// keyboard layout to be reloaded from the OS.
    ///
    /// # Safety
    ///
    /// `v` must be a live app.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_app_keyboard_changed(v: *mut App) {
        // SAFETY: forwarded from the caller.
        #[allow(unreachable_patterns)]
        if let Err(err) = unsafe { app(v) }.reload_keymap() {
            log::error!(target: LOG, "error reloading keyboard map err={err:?}");
        }
    }

    /// Open the configuration.
    ///
    /// # Safety
    ///
    /// `v` must be a live app.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_app_open_config(v: *mut App) {
        // SAFETY: forwarded from the caller.
        if let Err(err) = unsafe { app(v) }.perform_action(
            Target::App,
            Action::OpenConfig(apprt::action::OpenConfig::NewWindow),
        ) {
            log::error!(target: LOG, "error reloading config err={err}");
        }
    }

    /// Update the configuration to the provided config. This will propagate
    /// to all surfaces as well.
    ///
    /// # Safety
    ///
    /// `v` must be a live app and `config` a live config.
    #[cfg(all(target_os = "macos", not(test)))]
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_app_update_config(v: *mut App, config: *const Config) {
        // SAFETY: forwarded from the caller.
        let v = unsafe { app(v) };
        if let Err(err) = v.core_app().update_config(v, unsafe { &*config }) {
            log::error!(target: LOG, "error updating config err={err}");
        }
    }

    /// Returns true if the app needs to confirm quitting.
    ///
    /// # Safety
    ///
    /// `v` must be a live app.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_app_needs_confirm_quit(v: *mut App) -> bool {
        // SAFETY: forwarded from the caller.
        unsafe { app(v) }.core_app().needs_confirm_quit()
    }

    /// Returns true if the app has global keybinds.
    ///
    /// # Safety
    ///
    /// `v` must be a live app.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_app_has_global_keybinds(v: *mut App) -> bool {
        // SAFETY: forwarded from the caller.
        unsafe { app(v) }.has_global_keybinds()
    }

    /// Zig `std.enums.fromInt(apprt.ColorScheme, v)`.
    fn color_scheme_from_int(v: c_int) -> Option<ColorScheme> {
        match v {
            0 => Some(ColorScheme::Light),
            1 => Some(ColorScheme::Dark),
            _ => None,
        }
    }

    /// Update the color scheme of the app.
    ///
    /// # Safety
    ///
    /// `v` must be a live app.
    #[cfg(all(target_os = "macos", not(test)))]
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_app_set_color_scheme(v: *mut App, scheme_raw: c_int) {
        let Some(scheme) = color_scheme_from_int(scheme_raw) else {
            return;
        };

        // SAFETY: forwarded from the caller.
        let v = unsafe { app(v) };
        if let Err(err) = v.core_app().color_scheme_event(v, scheme) {
            log::error!(target: LOG, "error setting color scheme err={err}");
        }
    }

    /// Returns initial surface options.
    #[no_mangle]
    pub extern "C" fn ghostty_surface_config_new() -> SurfaceOptions {
        SurfaceOptions::default()
    }

    /// Create a new surface as part of an app.
    ///
    /// # Safety
    ///
    /// `v` must be a live app and `opts` valid options.
    #[cfg(all(target_os = "macos", not(test)))]
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_new(v: *mut App, opts: *const SurfaceOptions) -> *mut Surface {
        // SAFETY: forwarded from the caller.
        match unsafe { app(v).new_surface(&*opts) } {
            Ok(v) => v,
            Err(err) => {
                log::error!(target: LOG, "error initializing surface err={err}");
                std::ptr::null_mut()
            }
        }
    }

    /// # Safety
    ///
    /// `ptr` must be a live surface; it is freed.
    #[cfg(all(target_os = "macos", not(test)))]
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_free(ptr: *mut Surface) {
        // SAFETY: forwarded from the caller.
        unsafe { app((*ptr).app) }.close_surface(ptr);
    }

    /// Returns the userdata associated with the surface.
    ///
    /// # Safety
    ///
    /// `ptr` must be a live surface.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_userdata(ptr: *mut Surface) -> *mut c_void {
        // SAFETY: forwarded from the caller.
        unsafe { surface(ptr) }.userdata
    }

    /// Returns the app associated with a surface.
    ///
    /// # Safety
    ///
    /// `ptr` must be a live surface.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_app(ptr: *mut Surface) -> *mut App {
        // SAFETY: forwarded from the caller.
        unsafe { surface(ptr) }.app
    }

    /// Returns the config to use for surfaces that inherit from this one.
    ///
    /// # Safety
    ///
    /// `ptr` must be a live surface.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_inherited_config(
        ptr: *mut Surface,
        source: NewSurfaceContext,
    ) -> SurfaceOptions {
        // SAFETY: forwarded from the caller.
        unsafe { surface(ptr) }.new_surface_options(source)
    }

    /// Update the configuration to the provided config for only this surface.
    ///
    /// # Safety
    ///
    /// `ptr` must be a live surface and `config` a live config.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_update_config(ptr: *mut Surface, config: *const Config) {
        // SAFETY: forwarded from the caller.
        if let Err(err) = unsafe { surface(ptr) }.core_mut().update_config(unsafe { &*config }) {
            log::error!(target: LOG, "error updating config err={err}");
        }
    }

    /// Returns true if the surface needs to confirm quitting.
    ///
    /// # Safety
    ///
    /// `ptr` must be a live surface.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_needs_confirm_quit(ptr: *mut Surface) -> bool {
        // SAFETY: forwarded from the caller.
        unsafe { surface(ptr) }.core_mut().needs_confirm_quit()
    }

    /// Returns true if the surface process has exited.
    ///
    /// # Safety
    ///
    /// `ptr` must be a live surface.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_process_exited(ptr: *mut Surface) -> bool {
        // SAFETY: forwarded from the caller.
        unsafe { surface(ptr) }.core_mut().child_exited
    }

    /// Returns true if the surface has a selection.
    ///
    /// # Safety
    ///
    /// `ptr` must be a live surface.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_has_selection(ptr: *mut Surface) -> bool {
        // SAFETY: forwarded from the caller.
        unsafe { surface(ptr) }.core_mut().has_selection()
    }

    /// Same as ghostty_surface_read_text but reads from the user selection,
    /// if any.
    ///
    /// # Safety
    ///
    /// `ptr` must be a live surface and `result` writable.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_read_selection(ptr: *mut Surface, result: *mut Text) -> bool {
        // SAFETY: forwarded from the caller.
        let s = unsafe { surface(ptr) };
        let core_surface = s.core_mut();
        let _lock = RendererLock::new(core_surface);

        // If we don't have a selection, do nothing.
        let Some(core_sel) = core_surface.io.terminal.screens.active.selection else {
            return false;
        };

        // Read the text from the selection.
        // SAFETY: forwarded from the caller.
        read_text_locked(s, core_sel, unsafe { &mut *result })
    }

    /// Read some arbitrary text from the surface.
    ///
    /// This is an expensive operation so it shouldn't be called too
    /// often. We recommend that callers cache the result and throttle
    /// calls to this function.
    ///
    /// # Safety
    ///
    /// `ptr` must be a live surface and `result` writable.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_read_text(ptr: *mut Surface, sel: Selection, result: *mut Text) -> bool {
        // SAFETY: forwarded from the caller.
        let s = unsafe { surface(ptr) };
        let core_surface = s.core_mut();
        let _lock = RendererLock::new(core_surface);

        // SAFETY: the renderer state terminal is live while the lock is held.
        let screen = unsafe { &*core_surface.renderer_state.terminal.as_ptr() }.screens.active;
        let Some(core_sel) = sel.core(&screen) else {
            return false;
        };

        // SAFETY: forwarded from the caller.
        read_text_locked(s, core_sel, unsafe { &mut *result })
    }

    fn read_text_locked(surface: &Surface, core_sel: terminal::Selection, result: &mut Text) -> bool {
        let core_surface = surface.core_mut();

        // Get our text directly from the core surface.
        let text = match core_surface.dump_text_locked(core_sel) {
            Ok(v) => v,
            Err(err) => {
                log::warn!(target: LOG, "error reading text err={err}");
                return false;
            }
        };

        let vp = text.viewport.unwrap_or(core_surface::TextViewport {
            tl_px_x: -1.0,
            tl_px_y: -1.0,
            offset_start: 0,
            offset_len: 0,
        });

        // The text is handed to the caller NUL-terminated; it frees it
        // with ghostty_surface_free_text.
        let mut bytes = text.text.into_bytes();
        let text_len = bytes.len();
        bytes.push(0);
        let ptr = Box::into_raw(bytes.into_boxed_slice()) as *const c_char;

        *result = Text {
            tl_px_x: vp.tl_px_x,
            tl_px_y: vp.tl_px_y,
            offset_start: vp.offset_start,
            offset_len: vp.offset_len,
            text: ptr,
            text_len,
        };

        true
    }

    /// # Safety
    ///
    /// `ptr` must be text filled in by this API that wasn't freed.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_free_text(_surface: *mut Surface, ptr: *mut Text) {
        // SAFETY: forwarded from the caller.
        unsafe { (*ptr).deinit() };
    }

    /// Tell the surface that it needs to schedule a render
    ///
    /// # Safety
    ///
    /// `ptr` must be a live surface.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_refresh(ptr: *mut Surface) {
        // SAFETY: forwarded from the caller.
        unsafe { surface(ptr) }.refresh();
    }

    /// Tell the surface that it needs to schedule a render
    /// call as soon as possible (NOW if possible).
    ///
    /// # Safety
    ///
    /// `ptr` must be a live surface.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_draw(ptr: *mut Surface) {
        // SAFETY: forwarded from the caller.
        unsafe { surface(ptr) }.draw();
    }

    /// Update the size of a surface. This will trigger resize notifications
    /// to the pty and the renderer.
    ///
    /// # Safety
    ///
    /// `ptr` must be a live surface.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_set_size(ptr: *mut Surface, w: u32, h: u32) {
        // SAFETY: forwarded from the caller.
        unsafe { surface(ptr) }.update_size(w, h);
    }

    /// Return the size information a surface has.
    ///
    /// # Safety
    ///
    /// `ptr` must be a live surface.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_size(ptr: *mut Surface) -> SurfaceSize {
        // SAFETY: forwarded from the caller.
        let core = unsafe { surface(ptr) }.core_mut();
        let grid_size = core.size.grid();
        SurfaceSize {
            columns: grid_size.columns,
            rows: grid_size.rows,
            width_px: core.size.screen.width,
            height_px: core.size.screen.height,
            cell_width_px: core.size.cell.width,
            cell_height_px: core.size.cell.height,
        }
    }

    /// Returns the PID of the foreground process for the surface PTY.
    ///
    /// # Safety
    ///
    /// `ptr` must be a live surface.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_foreground_pid(ptr: *mut Surface) -> u64 {
        // SAFETY: forwarded from the caller.
        match unsafe { surface(ptr) }
            .core_mut()
            .get_process_info(crate::pty::ProcessInfo::ForegroundPid)
        {
            Some(crate::pty::ProcessInfoValue::ForegroundPid(v)) => v,
            _ => 0,
        }
    }

    /// Returns the PTY name for the surface. The returned string must be
    /// freed by the caller via ghostty_string_free.
    ///
    /// # Safety
    ///
    /// `ptr` must be a live surface.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_tty_name(ptr: *mut Surface) -> crate::main_c::String {
        // SAFETY: forwarded from the caller.
        match unsafe { surface(ptr) }
            .core_mut()
            .get_process_info(crate::pty::ProcessInfo::TtyName)
        {
            Some(crate::pty::ProcessInfoValue::TtyName(tty_name)) => {
                crate::main_c::String::from_vec_z(tty_name.as_bytes().to_vec())
            }
            _ => crate::main_c::String::EMPTY,
        }
    }

    /// Update the color scheme of the surface.
    ///
    /// # Safety
    ///
    /// `ptr` must be a live surface.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_set_color_scheme(ptr: *mut Surface, scheme_raw: c_int) {
        let Some(scheme) = color_scheme_from_int(scheme_raw) else {
            return;
        };
        // SAFETY: forwarded from the caller.
        unsafe { surface(ptr) }.color_scheme_callback(scheme);
    }

    /// Update the content scale of the surface.
    ///
    /// # Safety
    ///
    /// `ptr` must be a live surface.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_set_content_scale(ptr: *mut Surface, x: f64, y: f64) {
        // SAFETY: forwarded from the caller.
        unsafe { surface(ptr) }.update_content_scale(x, y);
    }

    /// Update the focused state of a surface.
    ///
    /// # Safety
    ///
    /// `ptr` must be a live surface.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_set_focus(ptr: *mut Surface, focused: bool) {
        // SAFETY: forwarded from the caller.
        unsafe { surface(ptr) }.focus_callback(focused);
    }

    /// Update the occlusion state of a surface.
    ///
    /// # Safety
    ///
    /// `ptr` must be a live surface.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_set_occlusion(ptr: *mut Surface, visible: bool) {
        // SAFETY: forwarded from the caller.
        unsafe { surface(ptr) }.occlusion_callback(visible);
    }

    /// Filter the mods if necessary. This handles settings such as
    /// `macos-option-as-alt`. The filtered mods should be used for
    /// key translation but should NOT be sent back via the `_key`
    /// function -- the original mods should be used for that.
    ///
    /// # Safety
    ///
    /// `ptr` must be a live surface.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_key_translation_mods(ptr: *mut Surface, mods_raw: c_int) -> c_int {
        // SAFETY: forwarded from the caller.
        let s = unsafe { surface(ptr) };
        let mods = mods_from_c(mods_raw);
        let option_as_alt = match s.core_mut().config.macos_option_as_alt {
            Some(v) => v,
            None => s.app().keyboard_layout().detect_option_as_alt(),
        };
        let result = mods.translation(option_as_alt);
        result.int() as c_int
    }

    /// Send this for raw keypresses (i.e. the keyDown event on macOS).
    /// This will handle the keymap translation and send the appropriate
    /// key and char events.
    ///
    /// # Safety
    ///
    /// `ptr` must be a live surface and the event text a valid C string
    /// or null.
    #[cfg(all(target_os = "macos", not(test)))]
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_key(ptr: *mut Surface, event: KeyEvent) -> bool {
        // SAFETY: forwarded from the caller.
        match unsafe { (*(*ptr).app).key_event(KeyTarget::Surface(ptr), event.key_event()) } {
            Ok(v) => v,
            Err(err) => {
                log::warn!(target: LOG, "error processing key event err={err}");
                false
            }
        }
    }

    /// Returns true if the given key event would trigger a binding
    /// if it were sent to the surface right now. The "right now"
    /// is important because things like trigger sequences are only
    /// valid until the next key event.
    ///
    /// # Safety
    ///
    /// `ptr` must be a live surface, the event text a valid C string or
    /// null, and `c_flags` null or writable.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_key_is_binding(
        ptr: *mut Surface,
        event: KeyEvent,
        c_flags: *mut input::binding::FlagsC,
    ) -> bool {
        // SAFETY: forwarded from the caller.
        let Some(core_event) = (unsafe { event.key_event() }).core() else {
            log::warn!(target: LOG, "error processing key event");
            return false;
        };

        // SAFETY: forwarded from the caller.
        let Some(flags) = unsafe { surface(ptr) }.core_mut().key_event_is_binding(core_event) else {
            return false;
        };
        if !c_flags.is_null() {
            // SAFETY: forwarded from the caller.
            unsafe { *c_flags = flags.cval() };
        }
        true
    }

    /// Send raw text to the terminal. This is treated like a paste
    /// so this isn't useful for sending escape sequences. For that,
    /// individual key input should be used.
    ///
    /// # Safety
    ///
    /// `ptr` must be a live surface and `text` point to `len` bytes.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_text(ptr: *mut Surface, text: *const u8, len: usize) {
        // SAFETY: forwarded from the caller.
        unsafe { surface(ptr).text_callback(c_slice(text, len)) };
    }

    /// Paste text into the terminal as if it came from the clipboard.
    /// Unlike ghostty_surface_text, this applies paste protection, so
    /// unsafe text (e.g. containing newlines) requests confirmation via
    /// the confirm_read_clipboard callback. The text must be null
    /// terminated and is copied as needed before this returns.
    ///
    /// # Safety
    ///
    /// `ptr` must be a live surface and `text` point to `len` bytes
    /// followed by a NUL.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_paste_text(ptr: *mut Surface, text: *const c_char, len: usize) {
        // SAFETY: forwarded from the caller.
        unsafe { surface(ptr).paste_text(c_slice(text as *const u8, len)) };
    }

    /// Set the preedit text for the surface. This is used for IME
    /// composition. If the length is 0, then the preedit text is cleared.
    ///
    /// # Safety
    ///
    /// `ptr` must be a live surface and `text` point to `len` bytes.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_preedit(ptr: *mut Surface, text: *const u8, len: usize) {
        // SAFETY: forwarded from the caller.
        unsafe {
            surface(ptr).preedit_callback(if len == 0 { None } else { Some(c_slice(text, len)) })
        };
    }

    /// Returns true if the surface currently has mouse capturing
    /// enabled.
    ///
    /// # Safety
    ///
    /// `ptr` must be a live surface.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_mouse_captured(ptr: *mut Surface) -> bool {
        // SAFETY: forwarded from the caller.
        unsafe { surface(ptr) }.core_mut().mouse_captured()
    }

    /// Tell the surface that it needs to schedule a render
    ///
    /// # Safety
    ///
    /// `ptr` must be a live surface.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_mouse_button(
        ptr: *mut Surface,
        action: input::MouseButtonState,
        button: input::MouseButton,
        mods: c_int,
    ) -> bool {
        // SAFETY: forwarded from the caller.
        unsafe { surface(ptr) }.mouse_button_callback(action, button, mods_from_c(mods))
    }

    /// Update the mouse position within the view.
    ///
    /// # Safety
    ///
    /// `ptr` must be a live surface.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_mouse_pos(ptr: *mut Surface, x: f64, y: f64, mods: c_int) {
        // SAFETY: forwarded from the caller.
        unsafe { surface(ptr) }.cursor_pos_callback(x, y, mods_from_c(mods));
    }

    /// # Safety
    ///
    /// `ptr` must be a live surface.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_mouse_scroll(ptr: *mut Surface, x: f64, y: f64, scroll_mods: c_int) {
        // SAFETY: forwarded from the caller.
        unsafe { surface(ptr) }.scroll_callback(x, y, scroll_mods_from_c(scroll_mods));
    }

    /// # Safety
    ///
    /// `ptr` must be a live surface.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_mouse_pressure(ptr: *mut Surface, stage_raw: u32, pressure: f64) {
        let stage = match stage_raw {
            0 => input::MousePressureStage::None,
            1 => input::MousePressureStage::Normal,
            2 => input::MousePressureStage::Deep,
            _ => return,
        };
        // SAFETY: forwarded from the caller.
        unsafe { surface(ptr) }.mouse_pressure_callback(stage, pressure);
    }

    /// # Safety
    ///
    /// `ptr` must be a live surface and the out pointers writable.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_ime_point(
        ptr: *mut Surface,
        x: *mut f64,
        y: *mut f64,
        width: *mut f64,
        height: *mut f64,
    ) {
        // SAFETY: forwarded from the caller.
        unsafe {
            let pos = surface(ptr).core_mut().ime_point();
            *x = pos.x;
            *y = pos.y;
            *width = pos.width;
            *height = pos.height;
        }
    }

    /// Request that the surface become closed. This will go through the
    /// normal trigger process that a close surface input binding would.
    ///
    /// # Safety
    ///
    /// `ptr` must be a live surface.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_request_close(ptr: *mut Surface) {
        // SAFETY: forwarded from the caller.
        let core = unsafe { surface(ptr) }.core_mut();
        // Prefer the binding path so close goes through the same Surface
        // hook as a close-surface keybind, rather than a raw close() that
        // bypasses it.
        let handled = match core.perform_binding_action(input::binding::Action::CloseSurface) {
            Ok(v) => v,
            Err(err) => {
                log::error!(target: LOG, "error requesting close err={err}");
                return;
            }
        };
        if !handled {
            core.close();
        }
    }

    /// Request that the surface split in the given direction.
    ///
    /// # Safety
    ///
    /// `ptr` must be a live surface.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_split(ptr: *mut Surface, direction: apprt::action::SplitDirection) {
        use apprt::action::SplitDirection as D;
        use input::binding::SplitDirection as B;

        // Menu shortcuts call this C API directly. Route through
        // performBindingAction so menus and keybinds share one path,
        // matching GTK.
        let action = input::binding::Action::NewSplit(match direction {
            D::Right => B::Right,
            D::Left => B::Left,
            D::Down => B::Down,
            D::Up => B::Up,
        });
        // SAFETY: forwarded from the caller.
        if let Err(err) = unsafe { surface(ptr) }.core_mut().perform_binding_action(action) {
            log::error!(target: LOG, "error creating new split err={err}");
        }
    }

    /// Focus on the next split (if any).
    ///
    /// # Safety
    ///
    /// `ptr` must be a live surface.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_split_focus(ptr: *mut Surface, direction: apprt::action::GotoSplit) {
        use apprt::action::GotoSplit as D;
        use input::binding::SplitFocusDirection as B;

        let action = input::binding::Action::GotoSplit(match direction {
            D::Previous => B::Previous,
            D::Next => B::Next,
            D::Up => B::Up,
            D::Down => B::Down,
            D::Left => B::Left,
            D::Right => B::Right,
        });
        // SAFETY: forwarded from the caller.
        if let Err(err) = unsafe { surface(ptr) }.core_mut().perform_binding_action(action) {
            log::error!(target: LOG, "error focusing split err={err}");
        }
    }

    /// Resize the current split by moving the split divider in the given
    /// direction. `direction` specifies which direction the split divider will
    /// move relative to the focused split. `amount` is a fractional value
    /// between 0 and 1 that specifies by how much the divider will move.
    ///
    /// # Safety
    ///
    /// `ptr` must be a live surface.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_split_resize(
        ptr: *mut Surface,
        direction: apprt::action::ResizeSplitDirection,
        amount: u16,
    ) {
        // SAFETY: forwarded from the caller.
        let s = unsafe { surface(ptr) };
        if let Err(err) = s.app().perform_action(
            Target::Surface(s.core_surface),
            Action::ResizeSplit(apprt::action::ResizeSplit { amount, direction }),
        ) {
            log::error!(target: LOG, "error resizing split err={err}");
        }
    }

    /// Equalize the size of all splits in the current window.
    ///
    /// # Safety
    ///
    /// `ptr` must be a live surface.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_split_equalize(ptr: *mut Surface) {
        // SAFETY: forwarded from the caller.
        let s = unsafe { surface(ptr) };
        if let Err(err) = s
            .app()
            .perform_action(Target::Surface(s.core_surface), Action::EqualizeSplits)
        {
            log::error!(target: LOG, "error equalizing splits err={err}");
        }
    }

    /// Invoke an action on the surface.
    ///
    /// # Safety
    ///
    /// `ptr` must be a live surface and `action_ptr` point to `action_len`
    /// bytes.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_binding_action(
        ptr: *mut Surface,
        action_ptr: *const u8,
        action_len: usize,
    ) -> bool {
        // SAFETY: forwarded from the caller.
        let action_str = unsafe { c_slice(action_ptr, action_len) };
        let action = match input::binding::Action::parse(action_str) {
            Ok(v) => v,
            Err(err) => {
                log::error!(
                    target: LOG,
                    "error parsing binding action action={} err={err:?}",
                    std::string::String::from_utf8_lossy(action_str)
                );
                return false;
            }
        };

        // SAFETY: forwarded from the caller.
        match unsafe { surface(ptr) }.core_mut().perform_binding_action(action.clone()) {
            Ok(v) => v,
            Err(err) => {
                log::error!(target: LOG, "error performing binding action action={action:?} err={err}");
                false
            }
        }
    }

    /// Complete a clipboard read request started via the read callback
    /// with the representations that could be served and, if requested,
    /// the listing of available MIME types. All memory is borrowed for
    /// the duration of the call. This can only be called once for a given
    /// request. Once it is called with a request the request pointer will
    /// be invalidated.
    ///
    /// To deny a request use ghostty_surface_deny_clipboard_request
    /// instead.
    ///
    /// # Safety
    ///
    /// `ptr` must be a live surface, `complete` valid and `state` a request
    /// handed out by a callback that wasn't completed yet.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_complete_clipboard_request(
        ptr: *mut Surface,
        complete: *const ClipboardComplete,
        state: *mut ClipboardRequest,
    ) {
        // SAFETY: forwarded from the caller; the state was leaked by us.
        unsafe { surface(ptr).complete_clipboard_request(&*complete, Box::from_raw(state)) };
    }

    /// Deny a clipboard read request started via the read callback,
    /// e.g. because the user rejected a confirmation prompt. Request
    /// types whose protocol expects an answer have their denial reply
    /// written to the pty. The request pointer is invalidated.
    ///
    /// # Safety
    ///
    /// `ptr` must be a live surface and `state` a request handed out by a
    /// callback that wasn't completed yet.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_deny_clipboard_request(ptr: *mut Surface, state: *mut ClipboardRequest) {
        // SAFETY: forwarded from the caller; the state was leaked by us.
        unsafe { surface(ptr).deny_clipboard_request(Box::from_raw(state)) };
    }

    /// # Safety
    ///
    /// `ptr` must be a live surface.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_inspector(ptr: *mut Surface) -> *mut Inspector {
        // SAFETY: forwarded from the caller.
        match unsafe { surface(ptr) }.init_inspector() {
            Ok(v) => v,
            Err(err) => {
                log::error!(target: LOG, "error initializing inspector err={err}");
                std::ptr::null_mut()
            }
        }
    }

    /// # Safety
    ///
    /// `ptr` must be a live surface.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_inspector_free(ptr: *mut Surface) {
        // SAFETY: forwarded from the caller.
        unsafe { surface(ptr) }.free_inspector();
    }

    /// # Safety
    ///
    /// `ptr` must be a live inspector.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_inspector_set_size(ptr: *mut Inspector, w: u32, h: u32) {
        // SAFETY: forwarded from the caller.
        unsafe { inspector(ptr) }.update_size(w, h);
    }

    /// # Safety
    ///
    /// `ptr` must be a live inspector.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_inspector_set_content_scale(ptr: *mut Inspector, x: f64, y: f64) {
        // SAFETY: forwarded from the caller.
        unsafe { inspector(ptr) }.update_content_scale(x, y);
    }

    /// # Safety
    ///
    /// `ptr` must be a live inspector.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_inspector_mouse_button(
        ptr: *mut Inspector,
        action: input::MouseButtonState,
        button: input::MouseButton,
        mods: c_int,
    ) {
        // SAFETY: forwarded from the caller.
        unsafe { inspector(ptr) }.mouse_button_callback(action, button, mods_from_c(mods));
    }

    /// # Safety
    ///
    /// `ptr` must be a live inspector.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_inspector_mouse_pos(ptr: *mut Inspector, x: f64, y: f64) {
        // SAFETY: forwarded from the caller.
        unsafe { inspector(ptr) }.cursor_pos_callback(x, y);
    }

    /// # Safety
    ///
    /// `ptr` must be a live inspector.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_inspector_mouse_scroll(ptr: *mut Inspector, x: f64, y: f64, scroll_mods: c_int) {
        // SAFETY: forwarded from the caller.
        unsafe { inspector(ptr) }.scroll_callback(x, y, scroll_mods_from_c(scroll_mods));
    }

    /// # Safety
    ///
    /// `ptr` must be a live inspector.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_inspector_key(
        ptr: *mut Inspector,
        action: input::Action,
        key: input::Key,
        c_mods: c_int,
    ) {
        // SAFETY: forwarded from the caller.
        if let Err(err) = unsafe { inspector(ptr) }.key_callback(action, key, mods_from_c(c_mods)) {
            log::error!(target: LOG, "error processing key event err={err}");
        }
    }

    /// # Safety
    ///
    /// `ptr` must be a live inspector and `str` a valid C string.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_inspector_text(ptr: *mut Inspector, str: *const c_char) {
        // SAFETY: forwarded from the caller.
        unsafe { inspector(ptr).text_callback(CStr::from_ptr(str)) };
    }

    /// # Safety
    ///
    /// `ptr` must be a live inspector.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_inspector_set_focus(ptr: *mut Inspector, focused: bool) {
        // SAFETY: forwarded from the caller.
        unsafe { inspector(ptr) }.focus_callback(focused);
    }

    /// Sets the window background blur on macOS to the desired value.
    /// I do this in Zig as an extern function because I don't know how to
    /// call these functions in Swift.
    ///
    /// This uses an undocumented, non-public API because this is what
    /// every terminal appears to use, including Terminal.app.
    ///
    /// # Safety
    ///
    /// `v` must be a live app and `window` an `NSWindow`.
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_set_window_background_blur(v: *mut App, window: *mut c_void) {
        // This is only supported on macOS
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (v, window);
        }

        #[cfg(target_os = "macos")]
        {
            // SAFETY: forwarded from the caller.
            let config = &unsafe { app(v) }.config;

            // Do nothing if we don't have background transparency enabled
            if config.background_opacity >= 1.0 {
                return;
            }

            let nswindow = objc::Object::from_id(window);
            // SAFETY: a valid NSWindow; the CGS functions accept any window
            // number of this process.
            unsafe {
                let window_number: usize = nswindow.msg_send(objc::sel(c"windowNumber"), ());
                let _ = CGSSetWindowBackgroundBlurRadius(
                    CGSDefaultConnectionForThread(),
                    window_number,
                    config.background_blur.cval() as c_int,
                );
            }
        }
    }

    // See ghostty_set_window_background_blur
    #[cfg(target_os = "macos")]
    extern "C" {
        fn CGSSetWindowBackgroundBlurRadius(conn: *mut c_void, window: usize, radius: c_int) -> i32;
        fn CGSDefaultConnectionForThread() -> *mut c_void;
    }

    // Darwin-only C APIs.

    /// # Safety
    ///
    /// `ptr` must be a live surface.
    #[cfg(target_os = "macos")]
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_set_display_id(ptr: *mut Surface, display_id: u32) {
        // SAFETY: forwarded from the caller.
        let core = unsafe { surface(ptr) }.core_mut();
        core.renderer_thread.mailbox.push(
            renderer::Message::MacosDisplayId(display_id),
            crate::datastruct::blocking_queue::Timeout::Forever,
        );
        let _ = core.renderer_thread.wakeup.notify();
    }

    /// This returns a CTFontRef that should be used for quicklook
    /// highlighted text. This is always the primary font in use
    /// regardless of the selected text. If coretext is not in use
    /// then this will return nothing.
    ///
    /// # Safety
    ///
    /// `ptr` must be a live surface.
    #[cfg(target_os = "macos")]
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_quicklook_font(ptr: *mut Surface) -> *mut c_void {
        use crate::pkg::macos::foundation::NumberType;
        use crate::pkg::macos::text::font_descriptor::attr;

        // SAFETY: forwarded from the caller.
        let s = unsafe { surface(ptr) };

        // We'll need content scale so fail early if we can't get it.
        let Ok(content_scale) = s.get_content_scale() else {
            return std::ptr::null_mut();
        };

        // Get the shared font grid. We acquire the lock to read the font
        // face (see the module deviations).
        // SAFETY: the font grid outlives the renderer.
        let grid = unsafe { &*s.core_mut().renderer.font_grid };
        let mut state = grid.lock.write().unwrap_or_else(|e| e.into_inner());

        let collection = &mut state.resolver.collection;
        let Ok(face) = collection.get_face(crate::font::collection::Index::default()) else {
            return std::ptr::null_mut();
        };

        // We need to unscale the content scale. We apply the
        // content scale to our font stack because we are rendering
        // at 1x but callers of this should be using scaled or apply
        // scale themselves.
        let size: f32 = match face.font.copy_attribute(attr::Size) {
            None => 12.0,
            Some(num) => {
                let mut v: f32 = 12.0;
                let _ = num.get_value(NumberType::Float, &mut v);
                num.release();
                v
            }
        };

        match face.font.copy_with_attributes(size / content_scale.y, None, None) {
            Ok(copy) => copy as *const _ as *mut c_void,
            Err(_) => std::ptr::null_mut(),
        }
    }

    /// This returns the selected word for quicklook. This will populate
    /// the buffer with the word under the cursor and the selection
    /// info so that quicklook can be rendered.
    ///
    /// This does not modify the selection active on the surface (if any).
    ///
    /// # Safety
    ///
    /// `ptr` must be a live surface and `result` writable.
    #[cfg(target_os = "macos")]
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_surface_quicklook_word(ptr: *mut Surface, result: *mut Text) -> bool {
        // SAFETY: forwarded from the caller.
        let s = unsafe { surface(ptr) };
        let core = s.core_mut();
        let _lock = RendererLock::new(core);

        // Get our word selection
        let sel = {
            // SAFETY: the renderer state terminal is live while locked.
            let screen = unsafe { &*core.renderer_state.terminal.as_ptr() }.screens.active;
            let Ok(pos) = s.get_cursor_pos() else {
                return false;
            };
            let pt_viewport = core.pos_to_viewport(pos.x as f64, pos.y as f64);
            let Some(pin) = screen.pages.pin(terminal::point::Point::Viewport(terminal::point::Coordinate {
                x: pt_viewport.x,
                y: pt_viewport.y,
            })) else {
                debug_assert!(false, "viewport pin out of bounds");
                return false;
            };
            let Some(sel) = core
                .io
                .terminal
                .screens
                .active
                .select_word(pin, &core.config.selection_word_chars)
            else {
                return false;
            };
            sel
        };

        // Read the selection
        // SAFETY: forwarded from the caller.
        read_text_locked(s, sel, unsafe { &mut *result })
    }

    /// # Safety
    ///
    /// `ptr` must be a live inspector and `device` an `MTLDevice` (+1).
    #[cfg(target_os = "macos")]
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_inspector_metal_init(ptr: *mut Inspector, device: objc::Id) -> bool {
        // SAFETY: forwarded from the caller.
        unsafe { inspector(ptr) }.init_metal(objc::Object::from_id(device))
    }

    /// # Safety
    ///
    /// `ptr` must be a live inspector, `command_buffer` an
    /// `MTLCommandBuffer` (+1) and `descriptor` an
    /// `MTLRenderPassDescriptor` (+1).
    #[cfg(target_os = "macos")]
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_inspector_metal_render(
        ptr: *mut Inspector,
        command_buffer: objc::Id,
        descriptor: objc::Id,
    ) {
        // SAFETY: forwarded from the caller.
        if let Err(err) = unsafe { inspector(ptr) }.render_metal(
            objc::Object::from_id(command_buffer),
            objc::Object::from_id(descriptor),
        ) {
            log::error!(target: LOG, "error rendering inspector err={err}");
        }
    }

    /// # Safety
    ///
    /// `ptr` must be a live inspector.
    #[cfg(target_os = "macos")]
    #[no_mangle]
    pub unsafe extern "C" fn ghostty_inspector_metal_shutdown(ptr: *mut Inspector) -> bool {
        // SAFETY: forwarded from the caller.
        let ptr = unsafe { inspector(ptr) };
        if let Some(v) = ptr.backend.take() {
            v.deinit();
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    #[test]
    fn surface_options_layout() {
        // ghostty_surface_config_s
        assert_eq!(0, offset_of!(SurfaceOptions, platform_tag));
        assert_eq!(8, offset_of!(SurfaceOptions, platform));
        assert_eq!(16, offset_of!(SurfaceOptions, userdata));
        assert_eq!(24, offset_of!(SurfaceOptions, scale_factor));
        assert_eq!(32, offset_of!(SurfaceOptions, font_size));
        assert_eq!(40, offset_of!(SurfaceOptions, working_directory));
        assert_eq!(64, offset_of!(SurfaceOptions, env_var_count));
        assert_eq!(80, offset_of!(SurfaceOptions, wait_after_command));
        assert_eq!(84, offset_of!(SurfaceOptions, context));
        assert_eq!(88, size_of::<SurfaceOptions>());

        let opts = capi::ghostty_surface_config_new();
        assert_eq!(0, opts.platform_tag);
        assert_eq!(1.0, opts.scale_factor);
        assert_eq!(0.0, opts.font_size);
        assert!(opts.working_directory.is_null());
        assert_eq!(NewSurfaceContext::Window, opts.context);
    }

    #[test]
    fn c_struct_sizes() {
        assert_eq!(64, size_of::<Options>());
        assert_eq!(32, size_of::<capi::KeyEvent>());
        assert_eq!(20, size_of::<capi::SurfaceSize>());
        assert_eq!(24, size_of::<capi::ClipboardContent>());
        assert_eq!(40, size_of::<capi::ClipboardComplete>());
        assert_eq!(48, size_of::<capi::ClipboardConfirm>());
        assert_eq!(40, size_of::<capi::Text>());
        assert_eq!(16, size_of::<capi::Point>());
        assert_eq!(36, size_of::<capi::Selection>());
    }

    #[test]
    fn platform_init_errors() {
        let c = PlatformC {
            macos: PlatformCMacOS {
                nsview: std::ptr::null_mut(),
            },
        };
        assert_eq!(PlatformError::InvalidEnumTag, Platform::init(0, c).unwrap_err());
        assert_eq!(PlatformError::InvalidEnumTag, Platform::init(3, c).unwrap_err());

        #[cfg(target_os = "macos")]
        {
            assert_eq!(PlatformError::NSViewMustBeSet, Platform::init(1, c).unwrap_err());
            assert_eq!(PlatformError::UIViewMustBeSet, Platform::init(2, c).unwrap_err());
        }
        #[cfg(not(target_os = "macos"))]
        {
            assert_eq!(PlatformError::UnsupportedPlatform, Platform::init(1, c).unwrap_err());
            assert_eq!(PlatformError::UnsupportedPlatform, Platform::init(2, c).unwrap_err());
        }
    }

    #[test]
    fn key_event_core() {
        let entry = input::keycodes::ENTRIES
            .iter()
            .find(|e| e.key == input::Key::KeyA)
            .expect("key a");

        let text = c"a";
        let c_event = capi::KeyEvent {
            action: input::Action::Press,
            mods: input::Mods { shift: true, ..Default::default() }.int() as c_int,
            consumed_mods: 0,
            keycode: entry.native,
            text: text.as_ptr(),
            unshifted_codepoint: 'a' as u32,
            composing: false,
        };
        // SAFETY: text is a valid C string.
        let event = unsafe { c_event.key_event() };
        assert!(event.mods.shift);
        assert!(!event.consumed_mods.shift);

        let core = event.core().unwrap();
        assert_eq!(input::Key::KeyA, core.key);
        assert_eq!(b"a", core.utf8);
        assert_eq!('a' as u32, core.unshifted_codepoint);

        // Unknown keycodes and out-of-range codepoints.
        let event = KeyEvent {
            keycode: u32::MAX,
            unshifted_codepoint: 0x20_0000,
            text: None,
            ..event
        };
        let core = event.core().unwrap();
        assert_eq!(input::Key::Unidentified, core.key);
        assert_eq!(0, core.unshifted_codepoint);
        assert_eq!(b"", core.utf8);
    }

    #[test]
    fn c_mods_truncate() {
        let mods = capi::mods_from_c(-1);
        assert!(mods.shift && mods.ctrl && mods.alt && mods.super_);

        let scroll = capi::scroll_mods_from_c(0x101 | (3 << 1));
        assert!(scroll.precision);
        assert_eq!(input::mouse::Momentum::Changed, scroll.momentum);
    }

    #[test]
    fn string_escape_round_trip() {
        let input: &[u8] = b"echo \"hi\"\n\t'x' \\ \x1b[0m\xff~";
        let mut escaped = Vec::new();
        string_escape(input, &mut escaped);
        assert_eq!(
            &b"echo \\\"hi\\\"\\n\\t'x' \\\\ \\x1b[0m\\xff~"[..],
            &escaped[..]
        );

        // The config's raw input parser recovers the original bytes.
        let len = crate::config::string::parse_in_place(&mut escaped).unwrap();
        assert_eq!(input, &escaped[..len]);
    }

    #[test]
    fn point_and_selection_pins() {
        let mut t = terminal::Terminal::init(terminal::terminal::Options {
            cols: 10,
            rows: 5,
            ..Default::default()
        })
        .unwrap();
        t.print_string(b"hello").unwrap();
        let screen = &*t.screens.active;

        let pt = capi::Point {
            tag: capi::PointTag::Active,
            coord_tag: capi::PointCoordTag::Exact,
            x: 100,
            y: 100,
        };
        // Clamped to the bottom-right cell.
        let pin = pt.pin(screen).unwrap();
        assert_eq!(9, pin.x);

        let tl = capi::Point {
            tag: capi::PointTag::Screen,
            coord_tag: capi::PointCoordTag::TopLeft,
            x: 0,
            y: 0,
        };
        let br = capi::Point {
            tag: capi::PointTag::Screen,
            coord_tag: capi::PointCoordTag::BottomRight,
            x: 0,
            y: 0,
        };
        let sel = capi::Selection {
            tl,
            br,
            rectangle: true,
        }
        .core(screen)
        .unwrap();
        assert!(sel.rectangle);
        assert_eq!(0, sel.start().x);
        assert_eq!(9, sel.end().x);
    }
}
