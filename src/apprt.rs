//! Port of `src/apprt.zig`.
//!
//! STATUS: ported
//!
//! "apprt" is the "application runtime" package. This abstracts the
//! application runtime and lifecycle management such as creating windows,
//! getting user input (mouse/keyboard), etc.
//!
//! This enables compile-time interfaces to be built to swap out the underlying
//! application runtime. For example: pure macOS Cocoa, GTK+, browser, etc.
//!
//! The goal is to have different implementations share as much of the core
//! logic as possible, and to only reach out to platform-specific implementation
//! code when absolutely necessary.
//!
//! Rust notes: Zig's `apprt.runtime` (the comptime-selected implementation
//! module) is [`rt`] because `runtime` is the module port of
//! `apprt/runtime.zig` (whose `Runtime` enum is re-exported here).
//!
//! [`App`]/[`Surface`] are the embedded runtime's types on macOS and the
//! GTK runtime's types when built with the `gtk` Cargo feature elsewhere.
//! Other builds (and all test builds) use the recording [`standin`] types; in test builds with `gtk` the stand-ins
//! wrap an optional real GTK app/surface and forward to it, so the core
//! test suite can observe what it asked of the runtime while GTK smoke
//! tests run the real apprt.

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "apprt/action.rs"]
pub mod action;
#[path = "apprt/browser.rs"]
pub mod browser;
#[path = "apprt/embedded.rs"]
pub mod embedded;
#[path = "apprt/gtk.rs"]
pub mod gtk;
#[path = "apprt/ipc.rs"]
pub mod ipc;
#[path = "apprt/none.rs"]
pub mod none;
#[path = "apprt/runtime.rs"]
pub mod runtime;
#[path = "apprt/structs.rs"]
pub mod structs;
#[path = "apprt/surface.rs"]
pub mod surface;
// @generated-mods-end

pub use self::action::{Action, Target};
pub use self::runtime::Runtime;

pub use self::structs::{
    Clipboard, ClipboardContent, ClipboardReadResult, ClipboardRequest, ClipboardRequestType,
    ColorScheme, ContentScale, CursorPos, IMEPos, Selection, SurfaceSize,
};

/// The implementation to use for the app runtime. This is comptime chosen
/// so that every build has exactly one application runtime implementation.
/// Note: it is very rare to use Runtime directly; most usage will use
/// Window or something.
///
/// Zig: `apprt.runtime`. The executable on Linux/FreeBSD uses GTK; the
/// library (macOS) uses the embedded runtime.
#[cfg(not(target_os = "macos"))]
pub use self::gtk as rt;
#[cfg(target_os = "macos")]
pub use self::embedded as rt;

/// The runtime app and surface (Zig: `apprt.runtime.App`/`Surface`).
/// On macOS this is the embedded runtime (libghostty).
#[cfg(all(target_os = "macos", not(test)))]
pub use self::embedded::{App, Surface};
#[cfg(all(feature = "gtk", not(target_os = "macos"), not(test)))]
pub use self::gtk::{App, Surface};
#[cfg(any(test, all(not(feature = "gtk"), not(target_os = "macos"))))]
pub use self::standin::{App, Surface};

/// An error from the runtime (Zig: the runtimes' inferred error sets).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

/// Recording stand-ins for the runtime's `App` and `Surface` types: the
/// runtime of builds without a GUI apprt (the `none` runtime) and of all
/// test builds.
///
/// They implement the subset of the apprt API that the core (`App.zig`,
/// `Surface.zig`, the renderer and termio) calls, with the same method
/// names as the Zig runtimes (in snake_case). They also record what the
/// core asked of them so tests can observe it. With the `gtk` feature, a
/// stand-in can wrap a real GTK runtime object (`rt`), to which it then
/// forwards every call (GTK smoke tests).
///
/// API notes (shared with the GTK runtime):
/// * `perform_action` takes `&self` (the app is shared with other
///   threads for `wakeup`) and fails with [`Error`].
/// * `clipboard_request` borrows the request; a runtime that keeps the
///   request (e.g. for a confirmation prompt) clones it, and hands it back
///   to `Surface::complete_clipboard_request` by reference.
pub mod standin {
    use std::collections::HashMap;
    use std::ffi::{CStr, CString};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use crate::apprt::action::{Action, ActionKey, Target};
    use crate::apprt::structs::{
        Clipboard, ClipboardContent, ClipboardContentBuf, ClipboardReadResult, ClipboardRequest,
        ContentScale, CursorPos, SurfaceSize,
    };
    use crate::command::EnvMap;
    use crate::config::Config;
    use crate::renderer;

    pub use super::Error;

    fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        m.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The recording runtime app.
    #[derive(Debug, Default)]
    pub struct App {
        /// The real GTK runtime app this stand-in forwards to, if any.
        #[cfg(all(feature = "gtk", test))]
        pub rt: Option<crate::apprt::gtk::App>,

        /// The app configuration (Zig: `App.config`).
        pub config: Config,

        /// The value `perform_action` returns (whether the runtime
        /// "handled" the action).
        pub handled: bool,

        /// The actions performed so far (stand-in bookkeeping).
        pub actions: Mutex<Vec<(Target, ActionKey)>>,

        /// The titles set via `set_title` (stand-in bookkeeping).
        pub titles: Mutex<Vec<CString>>,

        /// The number of `wakeup` calls (stand-in bookkeeping).
        pub wakeup_count: AtomicUsize,
    }

    // SAFETY: the recorded target pointers are only identifiers.
    unsafe impl Send for App {}
    unsafe impl Sync for App {}

    impl App {
        /// Wake up the app event loop so it processes its mailbox.
        pub fn wakeup(&self) {
            self.wakeup_count.fetch_add(1, Ordering::Relaxed);
            #[cfg(all(feature = "gtk", test))]
            if let Some(rt) = &self.rt {
                rt.wakeup();
            }
        }

        /// The number of wakeups so far.
        pub fn wakeups(&self) -> usize {
            self.wakeup_count.load(Ordering::Relaxed)
        }

        /// Perform a given action. Returns `true` if the action was able
        /// to be performed, `false` otherwise.
        pub fn perform_action(&self, target: Target, action: Action<'_>) -> Result<bool, Error> {
            if let Action::SetTitle(v) = &action {
                lock(&self.titles).push(v.title.to_owned());
            }
            lock(&self.actions).push((target, action.key()));
            #[cfg(all(feature = "gtk", test))]
            if let Some(rt) = &self.rt {
                return rt.perform_action(target, action);
            }
            Ok(self.handled)
        }

        /// Send the given IPC to a running Ghostty. Returns `true` if the
        /// action was able to be performed, `false` otherwise.
        ///
        /// IPC is static (it is called from CLI actions, not from a running
        /// app), so this forwards to the static `performIpc` of the
        /// platform's runtime: GTK with the `gtk` feature, embedded on
        /// macOS, and the `none` runtime otherwise.
        pub fn perform_ipc(
            target: &crate::apprt::ipc::Target<'_>,
            action: &crate::apprt::ipc::Action<'_>,
        ) -> Result<bool, crate::apprt::ipc::Errors> {
            #[cfg(all(feature = "gtk", not(target_os = "macos")))]
            return crate::apprt::gtk::App::perform_ipc(target, action);
            #[cfg(target_os = "macos")]
            return crate::apprt::embedded::App::perform_ipc(target, action);
            #[cfg(all(not(feature = "gtk"), not(target_os = "macos")))]
            return crate::apprt::none::App::perform_ipc(target, action);
        }

        /// Take the recorded action keys (stand-in bookkeeping).
        pub fn take_actions(&self) -> Vec<ActionKey> {
            std::mem::take(&mut *lock(&self.actions))
                .into_iter()
                .map(|(_, k)| k)
                .collect()
        }
    }

    /// The recording runtime surface.
    #[derive(Debug)]
    pub struct Surface {
        /// The real GTK runtime surface this stand-in forwards to, if any.
        #[cfg(all(feature = "gtk", test))]
        pub rt: Option<crate::apprt::gtk::Surface>,

        /// The core surface (Zig: `core_surface`).
        pub core_surface: *mut crate::surface::Surface,

        /// The runtime app.
        pub app: *mut App,

        /// The surface size in pixels.
        pub size: SurfaceSize,

        /// The content scale.
        pub content_scale: ContentScale,

        /// The cursor position.
        pub cursor_pos: CursorPos,

        /// The title.
        pub title: Option<CString>,

        /// Whether the selection clipboard is supported.
        pub supports_selection: bool,

        /// The environment `default_termio_env` returns (`None` uses the
        /// process environment).
        pub env: Option<EnvMap>,

        /// Clipboard contents set via `set_clipboard`.
        pub clipboards: Mutex<HashMap<Clipboard, Vec<ClipboardContentBuf>>>,

        /// Clipboard requests received.
        pub clipboard_requests: Mutex<Vec<(Clipboard, ClipboardRequest)>>,

        /// Set to `Some(process_active)` when `close` is called.
        pub closed: Mutex<Option<bool>>,
    }

    impl Default for Surface {
        fn default() -> Self {
            Surface {
                #[cfg(all(feature = "gtk", test))]
                rt: None,
                core_surface: std::ptr::null_mut(),
                app: std::ptr::null_mut(),
                size: SurfaceSize {
                    width: 800,
                    height: 600,
                },
                content_scale: ContentScale { x: 1.0, y: 1.0 },
                cursor_pos: CursorPos::default(),
                title: None,
                supports_selection: true,
                env: None,
                clipboards: Mutex::new(HashMap::new()),
                clipboard_requests: Mutex::new(Vec::new()),
                closed: Mutex::new(None),
            }
        }
    }

    impl Surface {
        /// The core surface.
        pub fn core(&self) -> *mut crate::surface::Surface {
            #[cfg(all(feature = "gtk", test))]
            if let Some(rt) = &self.rt {
                return rt.core();
            }
            self.core_surface
        }

        /// The runtime app (Zig: `rtApp`).
        pub fn rt_app(&self) -> *mut App {
            #[cfg(all(feature = "gtk", test))]
            if let Some(rt) = &self.rt {
                return rt.rt_app();
            }
            self.app
        }

        /// Deinitialize the surface (and its core surface).
        pub fn deinit(&mut self) {
            if !self.core_surface.is_null() {
                // SAFETY: the core surface is live until deinit.
                unsafe { (*self.core_surface).deinit() };
            }
        }

        pub fn get_content_scale(&self) -> Result<ContentScale, Error> {
            #[cfg(all(feature = "gtk", test))]
            if let Some(rt) = &self.rt {
                return rt.get_content_scale();
            }
            Ok(self.content_scale)
        }

        pub fn get_size(&self) -> Result<SurfaceSize, Error> {
            #[cfg(all(feature = "gtk", test))]
            if let Some(rt) = &self.rt {
                return rt.get_size();
            }
            Ok(self.size)
        }

        pub fn get_cursor_pos(&self) -> Result<CursorPos, Error> {
            #[cfg(all(feature = "gtk", test))]
            if let Some(rt) = &self.rt {
                return rt.get_cursor_pos();
            }
            Ok(self.cursor_pos)
        }

        pub fn get_title(&self) -> Option<&CStr> {
            #[cfg(all(feature = "gtk", test))]
            if let Some(rt) = &self.rt {
                return rt.get_title();
            }
            self.title.as_deref()
        }

        pub fn close(&self, process_active: bool) {
            #[cfg(all(feature = "gtk", test))]
            if let Some(rt) = &self.rt {
                rt.close(process_active);
            }
            *lock(&self.closed) = Some(process_active);
        }

        pub fn supports_clipboard(&self, clipboard: Clipboard) -> bool {
            match clipboard {
                Clipboard::Standard => true,
                Clipboard::Selection | Clipboard::Primary => self.supports_selection,
            }
        }

        pub fn set_clipboard(
            &self,
            clipboard: Clipboard,
            contents: &[ClipboardContent<'_>],
            _confirm: bool,
        ) -> Result<(), Error> {
            #[cfg(all(feature = "gtk", test))]
            if let Some(rt) = &self.rt {
                return rt.set_clipboard(clipboard, contents, _confirm);
            }
            let contents = contents
                .iter()
                .map(|c| {
                    Ok(ClipboardContentBuf {
                        mime: CString::new(c.mime).map_err(|e| Error(e.to_string()))?,
                        data: c.data.to_vec(),
                    })
                })
                .collect::<Result<Vec<_>, Error>>()?;
            lock(&self.clipboards).insert(clipboard, contents);
            Ok(())
        }

        /// The first clipboard representation set so far (stand-in
        /// bookkeeping).
        pub fn clipboard_data(&self, clipboard: Clipboard) -> Option<Vec<u8>> {
            lock(&self.clipboards)
                .get(&clipboard)
                .and_then(|v| v.first().map(|c| c.data.clone()))
        }

        pub fn clipboard_request(
            &self,
            clipboard: Clipboard,
            req: &ClipboardRequest,
        ) -> Result<ClipboardReadResult, Error> {
            #[cfg(all(feature = "gtk", test))]
            if let Some(rt) = &self.rt {
                return rt.clipboard_request(clipboard, req);
            }
            if !self.supports_clipboard(clipboard) {
                return Ok(ClipboardReadResult::Unsupported);
            }
            lock(&self.clipboard_requests).push((clipboard, req.clone()));
            Ok(ClipboardReadResult::Started)
        }

        pub fn default_termio_env(&self) -> Result<EnvMap, Error> {
            #[cfg(all(feature = "gtk", test))]
            if let Some(rt) = &self.rt {
                return rt.default_termio_env();
            }
            Ok(match &self.env {
                Some(env) => env.clone(),
                None => crate::global::environ_map(),
            })
        }

        pub fn render_device_options(&self) -> Result<renderer::DeviceOptions, Error> {
            #[cfg(all(feature = "gtk", test))]
            if let Some(rt) = &self.rt {
                return rt.render_device_options();
            }
            Ok(renderer::DeviceOptions::default())
        }
    }
}
