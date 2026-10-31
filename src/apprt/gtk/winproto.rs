//! Port of `src/apprt/gtk/winproto.zig`.
//!
//! STATUS: ported
//!
//! `build_options.wayland`/`build_options.x11` ([`crate::build_config::X11`],
//! [`crate::build_config::WAYLAND`]) are true for the GTK build (gtk4 is
//! built with both backends on the supported Linux targets), so both
//! protocol implementations are always compiled in.
//!
//! The parts of `class/window.zig`'s `Window` and
//! `class/global_shortcuts.zig`'s `GlobalShortcuts` that the winproto
//! implementations use are described by the [`ApprtWindow`] and
//! [`GlobalShortcuts`] traits below, which those classes implement.
#![cfg(feature = "gtk")]

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "winproto/blur_region.rs"]
pub mod blur_region;
#[path = "winproto/noop.rs"]
pub mod noop;
#[path = "winproto/wayland.rs"]
pub mod wayland;
#[path = "winproto/x11.rs"]
pub mod x11;
// @generated-mods-end

use std::rc::Rc;

use gtk4::gdk;

use super::key;
use crate::command::EnvMap;
use crate::config::config::{Config, WindowDecoration};
use crate::input::binding;
use crate::input::key_mods::Mods;

/// The parts of `class/window.zig`'s `Window` that the winproto
/// implementations use.
// NOTE(port): implemented by `class::window::Window` (through a weak
// reference, since the Wayland backend keeps it past the window's lifetime);
// a trait so the winproto tests can use a plain `gtk.Window`.
pub trait ApprtWindow {
    /// The window as a `gtk.Window` (Zig: `as(gtk.Window)`, `as(gtk.Native)`,
    /// `as(gtk.Widget)`).
    fn window(&self) -> gtk4::Window;

    /// `getWindowDecoration`
    fn get_window_decoration(&self) -> WindowDecoration;

    /// `getConfig().get()`
    fn get_config(&self) -> Option<Rc<Config>>;

    /// `isQuickTerminal`
    fn is_quick_terminal(&self) -> bool;
}

/// `GlobalShortcuts.BindFailed`
pub use super::class::global_shortcuts::BindFailed;

/// The parts of `class/global_shortcuts.zig`'s `GlobalShortcuts` that the
/// winproto shortcut backends use.
// NOTE(port): implemented by `class::global_shortcuts::GlobalShortcuts`; a
// trait so the Wayland hotkey backend tests can record the signals.
pub trait GlobalShortcuts {
    /// Emit the trigger signal.
    fn emit_trigger(&self, action: &binding::Action);

    /// Emit the bind-failed signal. Public for winproto shortcut backends.
    fn emit_bind_failed(&self, failure: &BindFailed);
}

/// The errors of the winproto implementations (an inferred error set in
/// Zig).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    XkbInitializationError,
    NotX11Surface,
    NoWaylandDisplay,
    NotWaylandSurface,
    NoWaylandSurface,
    RoundtripFailed,
    RequestFailed,
    PropertyNotFound,
    PropertyTypeMismatch,
    PropertyFormatMismatch,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for Error {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    None,
    Wayland,
    X11,
}

/// App-state for the underlying windowing protocol. There should be one
/// instance of this struct per application.
pub enum App {
    None(noop::App),
    Wayland(wayland::App),
    X11(x11::App),
}

impl App {
    pub fn init(gdk_display: &gdk::Display, app_id: &str, config: &Config) -> Result<App, Error> {
        // Zig tries each union field in declaration order.
        if let Some(v) = noop::App::init(gdk_display, app_id, config)? {
            return Ok(App::None(v));
        }
        if let Some(v) = wayland::App::init(gdk_display, app_id, config)? {
            return Ok(App::Wayland(v));
        }
        if let Some(v) = x11::App::init(gdk_display, app_id, config)? {
            return Ok(App::X11(v));
        }

        Ok(App::None(noop::App {}))
    }

    pub fn protocol(&self) -> Protocol {
        match self {
            App::None(_) => Protocol::None,
            App::Wayland(_) => Protocol::Wayland,
            App::X11(_) => Protocol::X11,
        }
    }

    pub fn deinit(&mut self) {
        match self {
            App::None(v) => v.deinit(),
            App::Wayland(v) => v.deinit(),
            App::X11(v) => v.deinit(),
        }
    }

    pub fn event_mods(&mut self, device: Option<&gdk::Device>, gtk_mods: gdk::ModifierType) -> Mods {
        let mods = match self {
            App::None(v) => v.event_mods(device, gtk_mods),
            App::Wayland(v) => v.event_mods(device, gtk_mods),
            App::X11(v) => v.event_mods(device, gtk_mods),
        };
        mods.unwrap_or_else(|| key::translate_mods(gtk_mods))
    }

    pub fn supports_quick_terminal(&self) -> bool {
        match self {
            App::None(v) => v.supports_quick_terminal(),
            App::Wayland(v) => v.supports_quick_terminal(),
            App::X11(v) => v.supports_quick_terminal(),
        }
    }

    /// Set up necessary support for the quick terminal that must occur
    /// *before* the window-level winproto object is created.
    ///
    /// Only has an effect on the Wayland backend, where the gtk4-layer-shell
    /// library is initialized.
    pub fn init_quick_terminal(&mut self, apprt_window: &dyn ApprtWindow) -> Result<(), Error> {
        match self {
            App::None(v) => v.init_quick_terminal(apprt_window),
            App::Wayland(v) => v.init_quick_terminal(apprt_window),
            App::X11(v) => v.init_quick_terminal(apprt_window),
        }
    }

    /// Bind all global keybinds through a mechanism native to the
    /// windowing protocol. Returns false if there is none; the caller
    /// should then fall back to the XDG desktop portal.
    pub fn bind_global_shortcuts(&mut self, shortcuts: Rc<dyn GlobalShortcuts>, config: &Config) -> bool {
        match self {
            App::None(v) => v.bind_global_shortcuts(shortcuts, config),
            App::Wayland(v) => v.bind_global_shortcuts(shortcuts, config),
            App::X11(v) => v.bind_global_shortcuts(shortcuts, config),
        }
    }

    /// Unbind all global keybinds bound via bindGlobalShortcuts.
    pub fn clear_global_shortcuts(&mut self) {
        match self {
            App::None(v) => v.clear_global_shortcuts(),
            App::Wayland(v) => v.clear_global_shortcuts(),
            App::X11(v) => v.clear_global_shortcuts(),
        }
    }
}

/// Per-Window state for the underlying windowing protocol.
///
/// In Wayland, the terminology used is "Surface" and for it, this is
/// really "Surface"-specific state. But Ghostty uses the term "Surface"
/// heavily to mean something completely different, so we use "Window" here
/// to better match what it generally maps to in the Ghostty codebase.
pub enum Window {
    None(noop::Window),
    Wayland(wayland::Window),
    X11(x11::Window),
}

impl Window {
    pub fn init(app: &mut App, apprt_window: Rc<dyn ApprtWindow>) -> Result<Window, Error> {
        Ok(match app {
            App::None(v) => Window::None(noop::Window::init(v, apprt_window)?),
            App::Wayland(v) => Window::Wayland(wayland::Window::init(v, apprt_window)?),
            App::X11(v) => Window::X11(x11::Window::init(v, apprt_window)?),
        })
    }

    pub fn deinit(&mut self) {
        match self {
            Window::None(v) => v.deinit(),
            Window::Wayland(v) => v.deinit(),
            Window::X11(v) => v.deinit(),
        }
    }

    pub fn resize_event(&mut self) -> Result<(), Error> {
        match self {
            Window::None(v) => v.resize_event(),
            Window::Wayland(v) => v.resize_event(),
            Window::X11(v) => v.resize_event(),
        }
    }

    pub fn sync_appearance(&mut self) -> Result<(), Error> {
        match self {
            Window::None(v) => v.sync_appearance(),
            Window::Wayland(v) => v.sync_appearance(),
            Window::X11(v) => v.sync_appearance(),
        }
    }

    pub fn client_side_decoration_enabled(&self) -> bool {
        match self {
            Window::None(v) => v.client_side_decoration_enabled(),
            Window::Wayland(v) => v.client_side_decoration_enabled(),
            Window::X11(v) => v.client_side_decoration_enabled(),
        }
    }

    pub fn add_subprocess_env(&mut self, env: &mut EnvMap) -> Result<(), Error> {
        match self {
            Window::None(v) => v.add_subprocess_env(env),
            Window::Wayland(v) => v.add_subprocess_env(env),
            Window::X11(v) => v.add_subprocess_env(env),
        }
    }

    pub fn set_urgent(&mut self, urgent: bool) -> Result<(), Error> {
        match self {
            Window::None(v) => v.set_urgent(urgent),
            Window::Wayland(v) => v.set_urgent(urgent),
            Window::X11(v) => v.set_urgent(urgent),
        }
    }
}

#[cfg(test)]
pub(crate) mod test_util {
    use super::*;
    use std::cell::Cell;

    /// A plain `gtk.Window` standing in for the apprt window in tests.
    pub struct TestWindow {
        pub window: gtk4::Window,
        pub decoration: Cell<WindowDecoration>,
        pub config: Option<Rc<Config>>,
        pub quick_terminal: bool,
    }

    impl ApprtWindow for TestWindow {
        fn window(&self) -> gtk4::Window {
            self.window.clone()
        }
        fn get_window_decoration(&self) -> WindowDecoration {
            self.decoration.get()
        }
        fn get_config(&self) -> Option<Rc<Config>> {
            self.config.clone()
        }
        fn is_quick_terminal(&self) -> bool {
            self.quick_terminal
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apprt::gtk::ext::test_gtk;
    use gtk4::prelude::*;

    #[test]
    fn app_and_window_on_test_display() {
        test_gtk::run(|| {
            let display = gdk::Display::default().unwrap();
            let config = Config::default();
            let mut app = App::init(&display, "com.mitchellh.ghostty-test", &config).unwrap();
            // xvfb is an X11 server.
            assert_eq!(app.protocol(), Protocol::X11);
            assert!(!app.supports_quick_terminal());

            let window = gtk4::Window::new();
            WidgetExt::realize(&window);
            let apprt_window = Rc::new(test_util::TestWindow {
                window: window.clone(),
                decoration: std::cell::Cell::new(WindowDecoration::Server),
                config: Some(Rc::new(Config::default())),
                quick_terminal: false,
            });
            let mut w = Window::init(&mut app, apprt_window.clone()).unwrap();
            assert!(!w.client_side_decoration_enabled());
            w.sync_appearance().unwrap();
            w.resize_event().unwrap();
            w.set_urgent(true).unwrap();
            let mut env = EnvMap::new();
            w.add_subprocess_env(&mut env).unwrap();
            assert!(env.get("WINDOWID").unwrap().parse::<u64>().unwrap() > 0);
            apprt_window.decoration.set(WindowDecoration::Client);
            assert!(w.client_side_decoration_enabled());
            w.sync_appearance().unwrap();
            w.deinit();
            window.destroy();
            app.deinit();
        });
    }
}
