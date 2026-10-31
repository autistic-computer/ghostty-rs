//! Port of `src/apprt/gtk/App.zig`.
//!
//! STATUS: ported
//!
//! This is the main entrypoint to the apprt for Ghostty. Ghostty will
//! initialize this in main to start the application..
//!
//! Rust notes: Zig initializes the runtime app in place (`init(self: *App,
//! ...)`) because the GObject `Application` keeps a pointer back to it.
//! [`App::init`] returns the runtime app boxed (as [`crate::apprt::App`],
//! which wraps this type in test builds) so that pointer is stable.
#![cfg(feature = "gtk")]

use std::fmt;

use crate::app::App as CoreApp;
use crate::apprt;
use crate::apprt::action::{Action, Target};
use crate::apprt::gtk::class::application::{self, Application};
use crate::apprt::gtk::ipc::new_tab::new_tab as ipc_new_tab;
use crate::apprt::gtk::ipc::new_window::new_window as ipc_new_window;
use crate::apprt::gtk::ipc::toggle_quick_terminal::toggle_quick_terminal as ipc_toggle_quick_terminal;
use crate::apprt::gtk::Surface;
use crate::apprt::ipc;

/// GTK application ID
pub const APPLICATION_ID: &str = crate::apprt::gtk::build::info::APPLICATION_ID;

/// GTK object path
pub const OBJECT_PATH: &str = crate::apprt::gtk::build::info::OBJECT_PATH;

/// Options for [`App::init`]. Required by the apprt interface but we don't
/// use it.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {}

/// See the module documentation.
pub struct App {
    /// The GObject Application instance
    pub app: Application,
}

// SAFETY: the GObject application is only touched on the main thread. The
// only method other threads call is `wakeup`, which only wakes the
// (thread-safe) default main context.
unsafe impl Send for App {}
unsafe impl Sync for App {}

impl fmt::Debug for App {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("gtk::App").finish_non_exhaustive()
    }
}

impl App {
    /// Create the runtime app for `core_app`. The returned box must stay
    /// alive (and in place) for as long as the application runs.
    pub fn init(
        core_app: &mut CoreApp,
        _opts: Options,
    ) -> Result<Box<apprt::App>, application::NewError> {
        let app = Application::new(core_app)?;
        let mut rt = Box::new(super::wrap_app(App { app: app.clone() }));
        app.set_rt(&mut *rt);
        Ok(rt)
    }

    pub fn run(&self) -> Result<(), application::RunError> {
        self.app.run()
    }

    pub fn terminate(&self) {
        // We force deinitialize the app. We don't unref because other things
        // tend to have a reference at this point, so this just forces the
        // disposal now.
        self.app.deinit();
    }

    /// Called by CoreApp to wake up the event loop.
    pub fn wakeup(&self) {
        Application::wakeup();
    }

    pub fn perform_action(&self, target: Target, action: Action<'_>) -> Result<bool, apprt::Error> {
        self.app.perform_action(target, action)
    }

    /// Send the given IPC to a running Ghostty. Returns `true` if the action
    /// was able to be performed, `false` otherwise.
    ///
    /// Note that this is a static function. Since this is called from a CLI
    /// app (or some other process that is not Ghostty) there is no
    /// full-featured apprt App to use.
    pub fn perform_ipc(target: &ipc::Target<'_>, action: &ipc::Action<'_>) -> Result<bool, ipc::Errors> {
        match action {
            ipc::Action::NewWindow(value) => ipc_new_window(*target, value),
            ipc::Action::NewTab(value) => ipc_new_tab(*target, value),
            ipc::Action::ToggleQuickTerminal => ipc_toggle_quick_terminal(*target),
        }
    }

    /// Redraw the inspector for the given surface.
    pub fn redraw_inspector(&self, surface: &Surface) {
        surface.redraw_inspector();
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::apprt::gtk::class::application::action_fns;
    use crate::apprt::gtk::class::overrides::Overrides;
    use crate::apprt::gtk::class::window::Window;
    use crate::apprt::gtk::ext::test_gtk;
    use crate::config::Command;
    use crate::input::binding::Action as BindingAction;
    use crate::renderer::opengl::device::test_context::egl_lock;

    #[test]
    fn constants() {
        assert!(APPLICATION_ID.starts_with("com.mitchellh.ghostty"));
        assert!(OBJECT_PATH.starts_with("/com/mitchellh/ghostty"));
    }

    #[test]
    fn perform_ipc_without_instance() {
        // No running instance on the (absent) session bus: the IPC fails
        // or reports that it wasn't performed, but never panics.
        let _ = App::perform_ipc(&ipc::Target::Class(c"com.mitchellh.ghostty-test-none"), &ipc::Action::ToggleQuickTerminal);
    }

    /// Not a Zig test: a GTK window with a real terminal surface (core
    /// surface, OpenGL renderer and a shell on a pty) on the GTK test
    /// thread, driven by iterating the main context and ticking the core
    /// app like `Application.run` does.
    #[test]
    fn window_runs_shell() {
        let _guard = egl_lock();
        let Some(text) = test_gtk::run(|| {
            let app = Application::default();
            let overrides = Overrides {
                command: Some(Command::Direct(
                    ["/bin/sh", "-c", "echo SMOKE-$((6*7)); sleep 30"].iter().map(|a| (*a).into()).collect(),
                )),
                ..Overrides::NONE
            };
            let win = Window::new(&app, Some("smoke"));
            action_fns::init_and_show_window(&app, &win, None, &overrides);

            let ctx = glib::MainContext::default();
            let start = Instant::now();
            let mut text = String::new();
            while start.elapsed() < Duration::from_secs(30) {
                while ctx.iteration(false) {}
                // SAFETY: the test app's runtime app is leaked.
                let rt = unsafe { &*app.rt() };
                let _ = app.core().tick(rt);

                if let Some(core) = win.get_active_surface().and_then(|s| s.core()) {
                    // SAFETY: the surface is alive while its window is.
                    let core = unsafe { &mut *core };
                    if core.perform_binding_action(BindingAction::SelectAll).unwrap_or(false) {
                        text = core.selection_string().unwrap_or_default();
                        if text.contains("SMOKE-42") {
                            break;
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(10));
            }

            // The surface is registered with the core app.
            assert_eq!(app.surfaces().len(), 1);
            assert_eq!(win.get_active_surface().as_ref(), app.surfaces().first());

            // Destroying the window frees the surface, which removes it from
            // the core app (in finalize).
            gtk4::prelude::GtkWindowExt::destroy(&win);
            drop(win);
            let start = Instant::now();
            while !app.surfaces().is_empty() && start.elapsed() < Duration::from_secs(5) {
                while ctx.iteration(false) {}
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(app.surfaces().is_empty(), "surface leaked after its window was destroyed");
            text
        }) else {
            return;
        };
        assert!(text.contains("SMOKE-42"), "{text:?}");
    }
}
