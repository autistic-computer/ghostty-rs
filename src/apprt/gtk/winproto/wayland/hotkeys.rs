//! Port of `src/apprt/gtk/winproto/wayland/Hotkeys.zig`.
//!
//! STATUS: ported
//!
//! Global shortcuts backed by the vicinae-hotkey Wayland protocol.
//!
//! Zig passes each `*Entry` to its hotkey's listener. The port gives each
//! entry an ID (the hotkey's user data); `Globals` queues the events and
//! [`Hotkeys`] handles them after dispatching (see `globals.rs`), looking the
//! entry up by ID in the shared entry list.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use wayland_client::QueueHandle;

use super::globals::vicinae::{vicinae_hotkey_manager_v1, vicinae_hotkey_v1};
use super::globals::{Globals, HotkeyData};
use crate::apprt::gtk::key;
use crate::apprt::gtk::winproto::{BindFailed, GlobalShortcuts};
use crate::config::config::Config;
use crate::input::binding::{self, Action, Trigger, TriggerKey};

const LOG: &str = "winproto_wayland_hotkeys";

struct Entry {
    id: u64,
    /// Null once the binding was denied or revoked.
    hotkey: Option<vicinae_hotkey_v1::VicinaeHotkeyV1>,
    trigger: Trigger,
    action: Action,
    shortcuts: Rc<dyn GlobalShortcuts>,
}

impl Entry {
    fn deinit(&mut self) {
        if let Some(hotkey) = self.hotkey.take() {
            hotkey.destroy();
        }
    }

    fn bind(
        &mut self,
        manager: &vicinae_hotkey_manager_v1::VicinaeHotkeyManagerV1,
        app_id: &str,
        qh: &QueueHandle<Globals>,
    ) {
        // The only time this will return an error is when
        // `trigger.key` is `catch_all`, which is guarded
        // in the public `bind` function
        let keysym = key::keysym_from_trigger(self.trigger).expect("trigger has a keysym");

        // Zig formats into a 256-byte buffer and sends "" if it overflows.
        let mut desc = self.action.to_string();
        if desc.len() >= 256 {
            desc.clear();
        }

        let mut modifiers = vicinae_hotkey_manager_v1::Modifiers::empty();
        if self.trigger.mods.shift {
            modifiers |= vicinae_hotkey_manager_v1::Modifiers::Shift;
        }
        if self.trigger.mods.ctrl {
            modifiers |= vicinae_hotkey_manager_v1::Modifiers::Ctrl;
        }
        if self.trigger.mods.alt {
            modifiers |= vicinae_hotkey_manager_v1::Modifiers::Alt;
        }
        if self.trigger.mods.super_ {
            modifiers |= vicinae_hotkey_manager_v1::Modifiers::Super;
        }

        let hotkey = manager.bind(
            keysym,
            modifiers,
            None,
            app_id.to_owned(),
            desc,
            qh,
            HotkeyData { id: self.id },
        );
        self.hotkey = Some(hotkey);
    }

    /// Destroys the hotkey and returns the failure to report (emitted by the
    /// caller once the entry list is no longer borrowed).
    fn fail(&mut self, message: String, revoked: bool) -> (Rc<dyn GlobalShortcuts>, BindFailed) {
        if let Some(hotkey) = self.hotkey.take() {
            hotkey.destroy();
        }

        (
            self.shortcuts.clone(),
            BindFailed {
                trigger: self.trigger,
                action: self.action.clone(),
                message,
                revoked,
            },
        )
    }
}

pub struct Hotkeys {
    app_id: String,
    entries: Rc<RefCell<Vec<Entry>>>,
    next_id: u64,
}

impl Hotkeys {
    pub fn init(app_id: &str) -> Hotkeys {
        Hotkeys {
            app_id: app_id.to_owned(),
            entries: Rc::default(),
            next_id: 1,
        }
    }

    /// The receiver for this instance's hotkey events, to register with
    /// `Shared::set_hotkey_sink`.
    pub fn sink(&self) -> super::globals::HotkeySink {
        let entries: Weak<RefCell<Vec<Entry>>> = Rc::downgrade(&self.entries);
        Box::new(move |id, event| {
            if let Some(entries) = entries.upgrade() {
                hotkey_listener(&entries, id, event);
            }
        })
    }

    /// Must leave the entries in a valid empty state: clear may still be
    /// called after deinit during application teardown.
    pub fn deinit(&mut self) {
        let mut entries = self.entries.borrow_mut();
        for entry in entries.iter_mut() {
            entry.deinit();
        }
        *entries = Vec::new();
    }

    pub fn clear(&mut self) {
        let mut entries = self.entries.borrow_mut();
        for entry in entries.iter_mut() {
            entry.deinit();
        }
        entries.clear();
    }

    pub fn bind(
        &mut self,
        manager: &vicinae_hotkey_manager_v1::VicinaeHotkeyManagerV1,
        shortcuts: Rc<dyn GlobalShortcuts>,
        config: &Config,
        qh: &QueueHandle<Globals>,
    ) {
        self.clear();

        let mut entries = self.entries.borrow_mut();
        for (trigger, value) in config.keybind.set.bindings.iter() {
            let leaf: binding::GenericLeaf<'_> = match value {
                binding::Value::Leader(_) => continue,
                binding::Value::Leaf(leaf) => leaf.generic(),
                binding::Value::LeafChained(leaf) => leaf.generic(),
            };
            if !leaf.flags.global {
                continue;
            }
            // Catch all global keybinds don't really make sense
            if trigger.key == TriggerKey::CatchAll {
                continue;
            }

            // Only single-action global keybinds are supported, as in the
            // portal implementation.
            let actions = leaf.actions_slice();
            if actions.len() != 1 {
                continue;
            }

            let id = self.next_id;
            self.next_id += 1;
            entries.push(Entry {
                id,
                hotkey: None,
                trigger: **trigger,
                action: actions[0].clone(),
                shortcuts: shortcuts.clone(),
            });
        }

        for entry in entries.iter_mut() {
            entry.bind(manager, &self.app_id, qh);
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.borrow().len()
    }
}

fn hotkey_listener(entries: &RefCell<Vec<Entry>>, id: u64, event: vicinae_hotkey_v1::Event) {
    // Look the entry up and act on it with the list borrowed; signals are
    // emitted after releasing it since their handlers may rebind.
    enum Emit {
        Trigger(Rc<dyn GlobalShortcuts>, Action),
        BindFailed(Rc<dyn GlobalShortcuts>, BindFailed),
    }

    let emit = {
        let mut entries = entries.borrow_mut();
        let Some(entry) = entries.iter_mut().find(|e| e.id == id) else { return };
        match event {
            vicinae_hotkey_v1::Event::Bound => {
                log::debug!(target: LOG, "hotkey bound action={}", entry.action);
                None
            }

            vicinae_hotkey_v1::Event::Denied { reason, message } => {
                log::warn!(
                    target: LOG,
                    "hotkey denied action={} reason={:?} message={}",
                    entry.action,
                    reason,
                    message,
                );
                let (shortcuts, failure) = entry.fail(message, false);
                Some(Emit::BindFailed(shortcuts, failure))
            }

            vicinae_hotkey_v1::Event::Revoked { reason, message } => {
                log::warn!(
                    target: LOG,
                    "hotkey revoked action={} reason={:?} message={}",
                    entry.action,
                    reason,
                    message,
                );
                let (shortcuts, failure) = entry.fail(message, true);
                Some(Emit::BindFailed(shortcuts, failure))
            }

            vicinae_hotkey_v1::Event::Pressed { .. } => {
                Some(Emit::Trigger(entry.shortcuts.clone(), entry.action.clone()))
            }
            vicinae_hotkey_v1::Event::Released { .. } => None,
        }
    };

    match emit {
        Some(Emit::Trigger(shortcuts, action)) => shortcuts.emit_trigger(&action),
        Some(Emit::BindFailed(shortcuts, failure)) => shortcuts.emit_bind_failed(&failure),
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell as Cell;

    #[derive(Default)]
    struct Recorder {
        triggers: Cell<Vec<String>>,
        failures: Cell<Vec<(String, bool)>>,
    }

    impl GlobalShortcuts for Recorder {
        fn emit_trigger(&self, action: &Action) {
            self.triggers.borrow_mut().push(action.to_string());
        }
        fn emit_bind_failed(&self, failure: &BindFailed) {
            self.failures
                .borrow_mut()
                .push((failure.message.clone(), failure.revoked));
        }
    }

    fn entry(id: u64, shortcuts: Rc<dyn GlobalShortcuts>) -> Entry {
        Entry {
            id,
            hotkey: None,
            trigger: Trigger::default(),
            action: Action::Ignore,
            shortcuts,
        }
    }

    #[test]
    fn listener_routes_events_by_id() {
        let recorder = Rc::new(Recorder::default());
        let mut hotkeys = Hotkeys::init("com.mitchellh.ghostty");
        hotkeys
            .entries
            .borrow_mut()
            .extend([entry(1, recorder.clone()), entry(2, recorder.clone())]);
        let sink = hotkeys.sink();

        sink(1, vicinae_hotkey_v1::Event::Bound);
        sink(1, vicinae_hotkey_v1::Event::Pressed { serial: 0, time: 0 });
        sink(2, vicinae_hotkey_v1::Event::Released { serial: 0, time: 0 });
        sink(
            2,
            vicinae_hotkey_v1::Event::Denied {
                reason: wayland_client::WEnum::Value(vicinae_hotkey_v1::DenyReason::AlreadyBound),
                message: "taken".into(),
            },
        );
        sink(
            1,
            vicinae_hotkey_v1::Event::Revoked {
                reason: wayland_client::WEnum::Value(vicinae_hotkey_v1::RevokeReason::Removed),
                message: String::new(),
            },
        );
        // Unknown IDs are ignored.
        sink(99, vicinae_hotkey_v1::Event::Pressed { serial: 0, time: 0 });

        assert_eq!(&*recorder.triggers.borrow(), &["ignore".to_owned()]);
        assert_eq!(
            &*recorder.failures.borrow(),
            &[("taken".to_owned(), false), (String::new(), true)]
        );

        assert_eq!(hotkeys.len(), 2);
        hotkeys.clear();
        assert_eq!(hotkeys.len(), 0);
        hotkeys.deinit();
        hotkeys.clear();
    }
}
