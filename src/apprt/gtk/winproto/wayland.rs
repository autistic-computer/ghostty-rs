//! Port of `src/apprt/gtk/winproto/wayland.zig`.
//!
//! STATUS: ported
//!
//! Wayland protocol implementation for the Ghostty GTK apprt.
//!
//! gtk4-layer-shell (`@import("gtk4-layer-shell")`) is
//! [`crate::pkg::gtk4_layer_shell`], which loads the library at runtime;
//! when it isn't available, [`layer_shell::is_supported`] reports false, so
//! the quick terminal is disabled exactly as on a compositor without
//! wlr-layer-shell. With the `gtk-layer-shell` Cargo feature the
//! `gtk4-layer-shell` crate links the system library at build time instead
//! (like Zig's static link, which also keeps it loaded before
//! libwayland-client).

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "wayland/globals.rs"]
pub mod globals;
#[path = "wayland/hotkeys.rs"]
pub mod hotkeys;
// @generated-mods-end

use std::rc::{Rc, Weak};

use gdk4_wayland::prelude::*;
use gtk4::gdk;
use gtk4::prelude::*;
use wayland_client::protocol::{wl_compositor, wl_display, wl_surface};
use wayland_protocols::ext::background_effect::v1::client::{
    ext_background_effect_manager_v1, ext_background_effect_surface_v1,
};
use wayland_protocols::xdg::activation::v1::client::{xdg_activation_token_v1, xdg_activation_v1};
use wayland_protocols_plasma::server_decoration::client::{
    org_kde_kwin_server_decoration, org_kde_kwin_server_decoration_manager,
};
use wayland_protocols_plasma::slide::client::{org_kde_kwin_slide, org_kde_kwin_slide_manager};

use self::globals::{ActivationTokenData, DecorationMode, Globals, Shared};
use self::hotkeys::Hotkeys;
use super::blur_region::BlurRegion;
use super::{ApprtWindow, Error, GlobalShortcuts};
use crate::apprt::gtk::gtk_version;
use crate::command::EnvMap;
use crate::config::config::{
    Config, QuickTerminalKeyboardInteractivity, QuickTerminalLayer, QuickTerminalPosition,
    QuickTerminalScreen, QuickTerminalSizeDimensions, WindowDecoration,
};
use crate::input::key_mods::Mods;

const LOG: &str = "winproto_wayland";

/// `@import("gtk4-layer-shell")`: [`crate::pkg::gtk4_layer_shell`], with
/// safe wrappers taking `gtk.Window`/`gdk.Monitor` objects.
pub mod layer_shell {
    use gtk4::gdk;

    pub use crate::pkg::gtk4_layer_shell::{KeyboardMode, ShellEdge, ShellLayer};

    /// `std.meta.tags(ShellEdge)`
    pub const SHELL_EDGE_TAGS: [ShellEdge; 4] =
        [ShellEdge::Left, ShellEdge::Right, ShellEdge::Top, ShellEdge::Bottom];

    #[cfg(feature = "gtk-layer-shell")]
    mod imp {
        use super::*;
        use gtk4_layer_shell::LayerShell;

        fn edge(e: ShellEdge) -> gtk4_layer_shell::Edge {
            match e {
                ShellEdge::Left => gtk4_layer_shell::Edge::Left,
                ShellEdge::Right => gtk4_layer_shell::Edge::Right,
                ShellEdge::Top => gtk4_layer_shell::Edge::Top,
                ShellEdge::Bottom => gtk4_layer_shell::Edge::Bottom,
            }
        }

        pub fn is_supported() -> bool {
            gtk4_layer_shell::is_supported()
        }
        pub fn get_protocol_version() -> u32 {
            gtk4_layer_shell::protocol_version()
        }
        pub fn init_for_window(window: &gtk4::Window) {
            window.init_layer_shell();
        }
        pub fn set_layer(window: &gtk4::Window, layer: ShellLayer) {
            window.set_layer(match layer {
                ShellLayer::Background => gtk4_layer_shell::Layer::Background,
                ShellLayer::Bottom => gtk4_layer_shell::Layer::Bottom,
                ShellLayer::Top => gtk4_layer_shell::Layer::Top,
                ShellLayer::Overlay => gtk4_layer_shell::Layer::Overlay,
            });
        }
        pub fn set_anchor(window: &gtk4::Window, e: ShellEdge, anchor_to_edge: bool) {
            window.set_anchor(edge(e), anchor_to_edge);
        }
        pub fn set_margin(window: &gtk4::Window, e: ShellEdge, margin_size: i32) {
            window.set_margin(edge(e), margin_size);
        }
        pub fn set_keyboard_mode(window: &gtk4::Window, mode: KeyboardMode) {
            window.set_keyboard_mode(match mode {
                KeyboardMode::None => gtk4_layer_shell::KeyboardMode::None,
                KeyboardMode::Exclusive => gtk4_layer_shell::KeyboardMode::Exclusive,
                KeyboardMode::OnDemand => gtk4_layer_shell::KeyboardMode::OnDemand,
            });
        }
        pub fn set_monitor(window: &gtk4::Window, monitor: Option<&gdk::Monitor>) {
            window.set_monitor(monitor);
        }
        pub fn set_namespace(window: &gtk4::Window, name: &str) {
            window.set_namespace(Some(name));
        }
    }

    #[cfg(not(feature = "gtk-layer-shell"))]
    mod imp {
        use super::*;
        use crate::pkg::gtk4_layer_shell as ls;
        use gtk4::glib::translate::ToGlibPtr;

        fn ptr(window: &gtk4::Window) -> *mut ls::GtkWindow {
            let p: *mut gtk4::ffi::GtkWindow = window.to_glib_none().0;
            p.cast()
        }

        pub fn is_supported() -> bool {
            ls::is_supported()
        }
        pub fn get_protocol_version() -> u32 {
            ls::get_protocol_version()
        }
        pub fn init_for_window(window: &gtk4::Window) {
            // SAFETY: a live GtkWindow.
            unsafe { ls::init_for_window(ptr(window)) }
        }
        pub fn set_layer(window: &gtk4::Window, layer: ShellLayer) {
            // SAFETY: a live GtkWindow.
            unsafe { ls::set_layer(ptr(window), layer) }
        }
        pub fn set_anchor(window: &gtk4::Window, e: ShellEdge, anchor_to_edge: bool) {
            // SAFETY: a live GtkWindow.
            unsafe { ls::set_anchor(ptr(window), e, anchor_to_edge) }
        }
        pub fn set_margin(window: &gtk4::Window, e: ShellEdge, margin_size: i32) {
            // SAFETY: a live GtkWindow.
            unsafe { ls::set_margin(ptr(window), e, margin_size) }
        }
        pub fn set_keyboard_mode(window: &gtk4::Window, mode: KeyboardMode) {
            // SAFETY: a live GtkWindow.
            unsafe { ls::set_keyboard_mode(ptr(window), mode) }
        }
        pub fn set_monitor(window: &gtk4::Window, monitor: Option<&gdk::Monitor>) {
            let m: *mut gdk::ffi::GdkMonitor = monitor.to_glib_none().0;
            // SAFETY: a live GtkWindow and a null or live GdkMonitor.
            unsafe { ls::set_monitor(ptr(window), m.cast()) }
        }
        pub fn set_namespace(window: &gtk4::Window, name: &str) {
            let name = std::ffi::CString::new(name.replace('\0', "")).unwrap_or_default();
            // SAFETY: a live GtkWindow.
            unsafe { ls::set_namespace(ptr(window), &name) }
        }
    }

    pub use imp::*;
}

/// Wayland state that contains application-wide Wayland objects (e.g. wl_display).
pub struct App {
    pub display: wl_display::WlDisplay,
    pub globals: Rc<Shared>,
    pub hotkeys: Hotkeys,
}

impl App {
    pub fn init(gdk_display: &gdk::Display, app_id: &str, _config: &Config) -> Result<Option<App>, Error> {
        let Some(gdk_wayland_display) = gdk_display.downcast_ref::<gdk4_wayland::WaylandDisplay>() else {
            return Ok(None);
        };

        let display = gdk_wayland_display.wl_display().ok_or(Error::NoWaylandDisplay)?;

        let globals = Globals::init(&display)?;

        let hotkeys = Hotkeys::init(app_id);
        globals.set_hotkey_sink(Some(hotkeys.sink()));

        Ok(Some(App {
            display,
            globals,
            hotkeys,
        }))
    }

    pub fn deinit(&mut self) {
        self.hotkeys.deinit();
        self.globals.set_hotkey_sink(None);
    }

    pub fn bind_global_shortcuts(&mut self, shortcuts: Rc<dyn GlobalShortcuts>, config: &Config) -> bool {
        let Some(manager) = self
            .globals
            .globals
            .borrow()
            .get::<globals::vicinae::vicinae_hotkey_manager_v1::VicinaeHotkeyManagerV1>()
        else {
            return false;
        };
        self.hotkeys.bind(&manager, shortcuts, config, &self.globals.qh);
        self.globals.flush();
        true
    }

    pub fn clear_global_shortcuts(&mut self) {
        self.hotkeys.clear();
    }

    pub fn event_mods(&mut self, _: Option<&gdk::Device>, _: gdk::ModifierType) -> Option<Mods> {
        None
    }

    pub fn supports_quick_terminal(&self) -> bool {
        if !layer_shell::is_supported() {
            log::warn!(
                target: LOG,
                "your compositor does not support the wlr-layer-shell protocol; disabling quick terminal"
            );
            return false;
        }

        true
    }

    pub fn init_quick_terminal(&mut self, apprt_window: &dyn ApprtWindow) -> Result<(), Error> {
        let window = apprt_window.window();
        layer_shell::init_for_window(&window);

        // Set target monitor based on config (null lets compositor decide)
        let monitor = resolve_quick_terminal_monitor(&self.globals, apprt_window);
        layer_shell::set_monitor(&window, monitor.as_ref());
        Ok(())
    }
}

/// Per-window (wl_surface) state for the Wayland protocol.
pub struct Window {
    apprt_window: Rc<dyn ApprtWindow>,

    /// The Wayland surface for this window.
    surface: wl_surface::WlSurface,

    /// The context from the app where we can load our Wayland interfaces.
    globals: Rc<Shared>,

    /// Object that controls background effects like background blur.
    bg_effect: Option<ext_background_effect_surface_v1::ExtBackgroundEffectSurfaceV1>,

    /// Object that controls the decoration mode (client/server/auto)
    /// of the window.
    decoration: Option<org_kde_kwin_server_decoration::OrgKdeKwinServerDecoration>,

    /// Object that controls the slide-in/slide-out animations of the
    /// quick terminal. Always null for windows other than the quick terminal.
    slide: Option<org_kde_kwin_slide::OrgKdeKwinSlide>,

    /// Object that, when present, denotes that the window is currently
    /// requesting attention from the user.
    ///
    /// (Zig clears it when the token's `done` event arrives; the port's
    /// handler destroys the token, and a destroyed token here is ignored.)
    activation_token: Option<xdg_activation_token_v1::XdgActivationTokenV1>,

    blur_region: BlurRegion,

    /// The `enter-monitor` handler of quick terminal windows.
    enter_monitor: Option<(gdk::Surface, glib::SignalHandlerId)>,
}

impl Window {
    pub fn init(app: &mut App, apprt_window: Rc<dyn ApprtWindow>) -> Result<Window, Error> {
        let gtk_window = apprt_window.window();
        let gdk_surface = gtk_window
            .upcast_ref::<gtk4::Native>()
            .surface()
            .ok_or(Error::NotWaylandSurface)?;

        // This should never fail, because if we're being called at this point
        // then we've already asserted that our app state is Wayland.
        let gdk_wl_surface = gdk_surface
            .downcast_ref::<gdk4_wayland::WaylandSurface>()
            .ok_or(Error::NoWaylandSurface)?;

        let wl_surface = gdk_wl_surface.wl_surface().ok_or(Error::NoWaylandSurface)?;

        let globals = app.globals.clone();
        let qh = globals.qh.clone();

        // Get our decoration object so we can control the
        // CSD vs SSD status of this surface.
        let deco = globals
            .globals
            .borrow()
            .get::<org_kde_kwin_server_decoration_manager::OrgKdeKwinServerDecorationManager>()
            .map(|mgr| mgr.create(&wl_surface, &qh, ()));

        let bg_effect = 'bg: {
            if gtk_version::runtime_at_least(4, 23, 3) {
                // GTK 4.23.3 added an official way to do background blur,
                // so we don't need to do anything on our own.
                break 'bg None;
            }

            let Some(mgr) = globals
                .globals
                .borrow()
                .get::<ext_background_effect_manager_v1::ExtBackgroundEffectManagerV1>()
            else {
                break 'bg None;
            };

            Some(mgr.get_background_effect(&wl_surface, &qh, ()))
        };

        let enter_monitor = if apprt_window.is_quick_terminal() {
            let weak_window: Weak<dyn ApprtWindow> = Rc::downgrade(&apprt_window);
            let weak_globals = Rc::downgrade(&globals);
            let id = gdk_surface.connect_enter_monitor(move |_, monitor| {
                let (Some(apprt_window), Some(globals)) = (weak_window.upgrade(), weak_globals.upgrade())
                else {
                    return;
                };
                entered_monitor(monitor, &*apprt_window, &globals);
            });
            Some((gdk_surface.clone(), id))
        } else {
            None
        };

        globals.flush();

        Ok(Window {
            apprt_window,
            surface: wl_surface,
            globals,
            decoration: deco,
            bg_effect,
            slide: None,
            activation_token: None,
            blur_region: BlurRegion::EMPTY,
            enter_monitor,
        })
    }

    pub fn deinit(&mut self) {
        self.blur_region.deinit();
        if let Some(bg) = self.bg_effect.take() {
            bg.destroy();
        }
        if let Some(deco) = self.decoration.take() {
            deco.release();
        }
        if let Some(slide) = self.slide.take() {
            slide.release();
        }
        if let Some((surface, id)) = self.enter_monitor.take() {
            surface.disconnect(id);
        }
        self.globals.flush();
    }

    pub fn resize_event(&mut self) -> Result<(), Error> {
        if let Err(err) = self.sync_blur() {
            log::error!(target: LOG, "failed to sync blur={err}");
        }
        Ok(())
    }

    pub fn sync_appearance(&mut self) -> Result<(), Error> {
        if let Err(err) = self.sync_blur() {
            log::error!(target: LOG, "failed to sync blur={err}");
        }
        if let Err(err) = self.sync_decoration() {
            log::error!(target: LOG, "failed to sync decoration={err}");
        }

        if self.apprt_window.is_quick_terminal() {
            if let Err(err) = self.sync_quick_terminal() {
                log::warn!(target: LOG, "failed to sync quick terminal appearance={err}");
            }
        }
        self.globals.flush();
        Ok(())
    }

    pub fn client_side_decoration_enabled(&self) -> bool {
        match self.get_decoration_mode() {
            DecorationMode::Client => true,
            // If we support SSDs, then we should *not* enable CSDs if we prefer SSDs.
            // However, if we do not support SSDs (e.g. GNOME) then we should enable
            // CSDs even if the user prefers SSDs.
            DecorationMode::Server => self
                .globals
                .globals
                .borrow()
                .get::<org_kde_kwin_server_decoration_manager::OrgKdeKwinServerDecorationManager>()
                .is_none(),
            DecorationMode::None => false,
            _ => unreachable!(),
        }
    }

    pub fn add_subprocess_env(&mut self, _env: &mut EnvMap) -> Result<(), Error> {
        Ok(())
    }

    pub fn set_urgent(&mut self, urgent: bool) -> Result<(), Error> {
        let Some(activation) = self.globals.globals.borrow().get::<xdg_activation_v1::XdgActivationV1>() else {
            return Ok(());
        };

        // If there already is a token, destroy and unset it
        if let Some(token) = self.activation_token.take() {
            token.destroy();
        }

        self.activation_token = if urgent {
            let token = activation.get_activation_token(
                &self.globals.qh,
                ActivationTokenData {
                    surface: self.surface.clone(),
                },
            );
            token.set_surface(&self.surface);
            token.commit();
            Some(token)
        } else {
            None
        };
        self.globals.flush();
        Ok(())
    }

    /// Update the blur state of the window.
    fn sync_blur(&mut self) -> Result<(), Error> {
        let (compositor, caps) = {
            let globals = self.globals.globals.borrow();
            (
                globals.get::<wl_compositor::WlCompositor>(),
                globals.state.bg_effect_capabilities,
            )
        };
        let Some(compositor) = compositor else { return Ok(()) };
        let Some(bg) = self.bg_effect.clone() else { return Ok(()) };
        if !caps.contains(globals::BgEffectCapability::Blur) {
            return Ok(());
        }

        let Some(config) = self.apprt_window.get_config() else {
            return Ok(());
        };
        let blur = config.background_blur;

        if !blur.enabled() {
            self.blur_region.deinit();
            bg.set_blur_region(None);
            return Ok(());
        }

        let region = BlurRegion::calc_for_window(
            &*self.apprt_window,
            self.client_side_decoration_enabled(),
            false,
        );

        if region.eql(&self.blur_region) {
            // Region didn't change. Don't do anything.
            return Ok(());
        }

        let wl_region = compositor.create_region(&self.globals.qh, ());
        for s in &region.slices {
            wl_region.add(s.x as i32, s.y as i32, s.width as i32, s.height as i32);
        }

        bg.set_blur_region(Some(&wl_region));
        wl_region.destroy();
        self.blur_region = region;
        Ok(())
    }

    fn sync_decoration(&mut self) -> Result<(), Error> {
        let Some(deco) = &self.decoration else { return Ok(()) };

        // The protocol requests uint instead of enum so we have
        // to convert it.
        deco.request_mode(u32::from(self.get_decoration_mode()));
        Ok(())
    }

    fn get_decoration_mode(&self) -> DecorationMode {
        match self.apprt_window.get_window_decoration() {
            WindowDecoration::Auto => self
                .globals
                .globals
                .borrow()
                .state
                .default_deco_mode
                .unwrap_or(DecorationMode::Client),
            WindowDecoration::Client => DecorationMode::Client,
            WindowDecoration::Server => DecorationMode::Server,
            WindowDecoration::None => DecorationMode::None,
        }
    }

    fn sync_quick_terminal(&mut self) -> Result<(), Error> {
        let window = self.apprt_window.window();
        let Some(config) = self.apprt_window.get_config() else {
            return Ok(());
        };

        layer_shell::set_layer(
            &window,
            match config.gtk_quick_terminal_layer {
                QuickTerminalLayer::Overlay => layer_shell::ShellLayer::Overlay,
                QuickTerminalLayer::Top => layer_shell::ShellLayer::Top,
                QuickTerminalLayer::Bottom => layer_shell::ShellLayer::Bottom,
                QuickTerminalLayer::Background => layer_shell::ShellLayer::Background,
            },
        );
        layer_shell::set_namespace(&window, config.gtk_quick_terminal_namespace.as_str());

        // Re-resolve the target monitor on every sync so that config reloads
        // and primary-output changes take effect without recreating the window.
        let target_monitor = resolve_quick_terminal_monitor(&self.globals, &*self.apprt_window);
        layer_shell::set_monitor(&window, target_monitor.as_ref());

        layer_shell::set_keyboard_mode(
            &window,
            match config.quick_terminal_keyboard_interactivity {
                QuickTerminalKeyboardInteractivity::None => layer_shell::KeyboardMode::None,
                QuickTerminalKeyboardInteractivity::OnDemand => {
                    if layer_shell::get_protocol_version() < 4 {
                        log::warn!(
                            target: LOG,
                            "your compositor does not support on-demand keyboard access; falling back to exclusive access"
                        );
                        layer_shell::KeyboardMode::Exclusive
                    } else {
                        layer_shell::KeyboardMode::OnDemand
                    }
                }
                QuickTerminalKeyboardInteractivity::Exclusive => layer_shell::KeyboardMode::Exclusive,
            },
        );

        let anchored_edge: Option<layer_shell::ShellEdge> = match config.quick_terminal_position {
            QuickTerminalPosition::Left => Some(layer_shell::ShellEdge::Left),
            QuickTerminalPosition::Right => Some(layer_shell::ShellEdge::Right),
            QuickTerminalPosition::Top => Some(layer_shell::ShellEdge::Top),
            QuickTerminalPosition::Bottom => Some(layer_shell::ShellEdge::Bottom),
            QuickTerminalPosition::Center => None,
        };

        for edge in layer_shell::SHELL_EDGE_TAGS {
            if anchored_edge == Some(edge) {
                layer_shell::set_margin(&window, edge, 0);
                layer_shell::set_anchor(&window, edge, true);
                continue;
            }

            // Arbitrary margin - could be made customizable?
            layer_shell::set_margin(&window, edge, 20);
            layer_shell::set_anchor(&window, edge, false);
        }

        if let Some(slide) = self.slide.take() {
            slide.release();
        }

        self.slide = match anchored_edge {
            Some(anchored) => 'slide: {
                let Some(mgr) = self
                    .globals
                    .globals
                    .borrow()
                    .get::<org_kde_kwin_slide_manager::OrgKdeKwinSlideManager>()
                else {
                    break 'slide None;
                };

                let slide = mgr.create(&self.surface, &self.globals.qh, ());

                let slide_location = match anchored {
                    layer_shell::ShellEdge::Top => org_kde_kwin_slide::Location::Top,
                    layer_shell::ShellEdge::Bottom => org_kde_kwin_slide::Location::Bottom,
                    layer_shell::ShellEdge::Left => org_kde_kwin_slide::Location::Left,
                    layer_shell::ShellEdge::Right => org_kde_kwin_slide::Location::Right,
                };

                slide.set_location(u32::from(slide_location));
                slide.commit();
                Some(slide)
            }
            None => None,
        };
        Ok(())
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        self.deinit();
    }
}

/// Update the size of the quick terminal based on monitor dimensions.
fn entered_monitor(monitor: &gdk::Monitor, apprt_window: &dyn ApprtWindow, globals: &Shared) {
    let window = apprt_window.window();
    let Some(config) = apprt_window.get_config() else { return };

    let resolved_monitor = resolve_quick_terminal_monitor(globals, apprt_window);

    // Use the configured monitor for sizing if not in mouse mode.
    let size_monitor = resolved_monitor.as_ref().unwrap_or(monitor);

    let monitor_size = size_monitor.geometry();

    let dims = config.quick_terminal_size.calculate(
        config.quick_terminal_position,
        QuickTerminalSizeDimensions {
            width: monitor_size.width() as u32,
            height: monitor_size.height() as u32,
        },
    );

    window.set_default_size(dims.width as i32, dims.height as i32);
}

/// Resolve the quick-terminal-screen config to a specific monitor.
/// Returns null to let the compositor decide (used for .mouse mode).
/// Caller owns the returned ref and must unref it.
fn resolve_quick_terminal_monitor(globals: &Shared, apprt_window: &dyn ApprtWindow) -> Option<gdk::Monitor> {
    let config = apprt_window.get_config()?;

    match config.quick_terminal_screen {
        QuickTerminalScreen::Mouse => return None,
        QuickTerminalScreen::Main | QuickTerminalScreen::MacosMenuBar => {}
    }

    let display = apprt_window.window().upcast_ref::<gtk4::Widget>().display();
    let monitors = display.monitors();

    // Try to find the monitor matching the primary output name.
    let primary = globals.globals.borrow().state.primary_output_name.clone();
    if let Some(stored_name) = primary {
        let mut i = 0;
        while let Some(item) = monitors.item(i) {
            i += 1;
            let Ok(monitor) = item.downcast::<gdk::Monitor>() else { continue };
            if monitor.connector().is_some_and(|c| c.as_str() == stored_name) {
                return Some(monitor);
            }
        }
    }

    // Fall back to the first monitor in the list.
    monitors.item(0)?.downcast::<gdk::Monitor>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_shell_without_library() {
        if cfg!(not(feature = "gtk-layer-shell")) {
            assert!(!layer_shell::is_supported());
            assert_eq!(layer_shell::get_protocol_version(), 0);
        }
        assert_eq!(layer_shell::SHELL_EDGE_TAGS.len(), 4);
    }
}
