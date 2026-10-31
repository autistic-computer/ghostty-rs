//! Port of `src/apprt/gtk/winproto/noop.zig`.
//!
//! STATUS: ported

use std::rc::Rc;

use gtk4::gdk;

use super::{ApprtWindow, Error, GlobalShortcuts};
use crate::command::EnvMap;
use crate::config::config::Config;
use crate::input::key_mods::Mods;

#[derive(Debug, Default)]
pub struct App {}

impl App {
    pub fn init(_: &gdk::Display, _: &str, _: &Config) -> Result<Option<App>, Error> {
        Ok(None)
    }

    pub fn deinit(&mut self) {}

    pub fn event_mods(&mut self, _: Option<&gdk::Device>, _: gdk::ModifierType) -> Option<Mods> {
        None
    }

    pub fn supports_quick_terminal(&self) -> bool {
        false
    }

    pub fn init_quick_terminal(&mut self, _: &dyn ApprtWindow) -> Result<(), Error> {
        Ok(())
    }

    pub fn bind_global_shortcuts(&mut self, _: Rc<dyn GlobalShortcuts>, _: &Config) -> bool {
        false
    }

    pub fn clear_global_shortcuts(&mut self) {}
}

#[derive(Debug, Default)]
pub struct Window {}

impl Window {
    pub fn init(_: &mut App, _: Rc<dyn ApprtWindow>) -> Result<Window, Error> {
        Ok(Window {})
    }

    pub fn deinit(&mut self) {}

    /// `updateConfigEvent(*const ApprtWindow.DerivedConfig)`; the derived
    /// config type belongs to the unported `class/window.zig`, so the port
    /// takes no argument.
    pub fn update_config_event(&mut self) -> Result<(), Error> {
        Ok(())
    }

    pub fn resize_event(&mut self) -> Result<(), Error> {
        Ok(())
    }

    pub fn sync_appearance(&mut self) -> Result<(), Error> {
        Ok(())
    }

    /// This returns true if CSD is enabled for this window. This
    /// should be the actual present state of the window, not the
    /// desired state.
    pub fn client_side_decoration_enabled(&self) -> bool {
        true
    }

    pub fn add_subprocess_env(&mut self, _: &mut EnvMap) -> Result<(), Error> {
        Ok(())
    }

    pub fn set_urgent(&mut self, _: bool) -> Result<(), Error> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_window_defaults() {
        let mut app = App::default();
        assert!(!app.supports_quick_terminal());
        assert!(app.event_mods(None, gdk::ModifierType::SHIFT_MASK).is_none());
        let mut w = Window::default();
        assert!(w.client_side_decoration_enabled());
        let mut env = EnvMap::new();
        w.add_subprocess_env(&mut env).unwrap();
        assert!(env.is_empty());
        w.set_urgent(true).unwrap();
        w.update_config_event().unwrap();
        app.clear_global_shortcuts();
    }
}
