//! Port of `src/apprt/gtk/class/global_shortcuts.zig`.
//!
//! STATUS: ported.
//!
//! Deviations:
//! * The per-refresh arena is gone; `arena` only records whether a
//!   refresh set up portal state (Zig: `arena != null`), and the map owns
//!   its strings and actions.
//! * The portal payloads are built with the GVariant builders instead of
//!   `g_variant_new_parsed` format strings (same types and values).
//! * D-Bus callbacks hold a weak reference to the object (Zig passes an
//!   unowned pointer); the subscriptions are dropped in `close` as in Zig.
//! * Building the trigger map is factored into [`build_map`] so that it can
//!   be tested without a D-Bus connection.
#![cfg(feature = "gtk")]
// `signal_subscribe`/`signal_unsubscribe` mirror the Zig calls directly.
#![allow(deprecated)]

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::LazyLock;

use glib::prelude::*;
use glib::subclass::prelude::*;
use glib::subclass::Signal;
use indexmap::IndexMap;

use crate::apprt::gtk::class::{self, accessor, Config};
use crate::apprt::gtk::key;
use crate::apprt::gtk::winproto;
use crate::config::config::Config as CoreConfig;
use crate::input::binding::{self, Action, Trigger};

use crate::apprt::gtk::class::application::Application;

const LOG: &str = "gtk_ghostty_global_shortcuts";

/// Property names (Zig: `properties`).
pub mod properties {
    pub const CONFIG: &str = "config";
    pub const DBUS_CONNECTION: &str = "dbus-connection";
}

/// Signal names (Zig: `signals`).
pub mod signals {
    /// Emitted whenever a global shortcut is triggered. The parameter is a
    /// `G_TYPE_POINTER` to an [`crate::input::binding::Action`].
    pub const TRIGGER: &str = "trigger";

    /// Emitted when a global shortcut fails to bind or is revoked.
    pub const BIND_FAILED: &str = "bind-failed";
}

/// A global shortcut that failed to bind or was revoked.
/// Only valid for the duration of the signal emission.
#[derive(Debug, Clone, glib::Boxed)]
#[boxed_type(name = "GhosttyGlobalShortcutsBindFailed")]
pub struct BindFailed {
    pub trigger: Trigger,
    pub action: Action,

    /// May be empty.
    pub message: String,

    /// Revoked after binding rather than denied up front.
    pub revoked: bool,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct GlobalShortcuts {
        /// The configuration that this is using.
        pub config: RefCell<Option<Config>>,

        /// The dbus connection.
        pub dbus_connection: RefCell<Option<gio::DBusConnection>>,

        /// Whether a refresh set up portal state (Zig: an arena allocator
        /// that is present for each refresh).
        pub arena: Cell<bool>,

        /// A mapping from a unique ID to an action.
        /// Currently the unique ID is simply the serialized representation of the
        /// trigger that was used for the action as triggers are unique in the keymap,
        /// but this may change in the future.
        pub map: RefCell<IndexMap<String, Action>>,

        /// The handle of the current global shortcuts portal session,
        /// as a D-Bus object path.
        pub handle: RefCell<Option<String>>,

        /// The D-Bus signal subscription for the response signal on requests.
        pub response_subscription: RefCell<Option<gio::SignalSubscriptionId>>,

        /// The D-Bus signal subscription for the keybind activate signal.
        pub activate_subscription: RefCell<Option<gio::SignalSubscriptionId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for GlobalShortcuts {
        const NAME: &'static str = "GhosttyGlobalShortcuts";
        type Type = super::GlobalShortcuts;
        type ParentType = glib::Object;
    }

    impl ObjectImpl for GlobalShortcuts {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPS: LazyLock<Vec<glib::ParamSpec>> = LazyLock::new(|| {
                vec![
                    glib::ParamSpecObject::builder::<Config>(properties::CONFIG).build(),
                    glib::ParamSpecObject::builder::<gio::DBusConnection>(
                        properties::DBUS_CONNECTION,
                    )
                    .build(),
                ]
            });
            PROPS.as_ref()
        }

        fn property(&self, _: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                properties::CONFIG => accessor::get(&self.config),
                properties::DBUS_CONNECTION => accessor::get(&self.dbus_connection),
                _ => unreachable!(),
            }
        }

        fn set_property(&self, _: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            match pspec.name() {
                properties::CONFIG => accessor::set(&self.config, value),
                properties::DBUS_CONNECTION => {
                    let conn: Option<gio::DBusConnection> = value.get().unwrap();
                    self.obj().set_dbus_connection(conn.as_ref());
                }
                _ => unreachable!(),
            }
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: LazyLock<Vec<Signal>> = LazyLock::new(|| {
                vec![
                    Signal::builder(signals::TRIGGER).param_types([glib::Type::POINTER]).build(),
                    Signal::builder(signals::BIND_FAILED)
                        .param_types([BindFailed::static_type()])
                        .build(),
                ]
            });
            SIGNALS.as_ref()
        }

        fn constructed(&self) {
            self.parent_constructed();
            // Zig connects this in instance init.
            self.obj().connect_notify_local(Some(properties::CONFIG), |this, _| {
                this.refresh();
            });
        }

        fn dispose(&self) {
            // Since we drop references here we may lose access to things like
            // dbus connections, so we need to close all our connections right
            // away instead of in finalize.
            self.obj().close();

            self.config.replace(None);
            self.dbus_connection.replace(None);
        }
    }
}

glib::wrapper! {
    pub struct GlobalShortcuts(ObjectSubclass<imp::GlobalShortcuts>);
}

impl Default for GlobalShortcuts {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl winproto::GlobalShortcuts for GlobalShortcuts {
    fn emit_trigger(&self, action: &Action) {
        GlobalShortcuts::emit_trigger(self, action);
    }

    fn emit_bind_failed(&self, failure: &BindFailed) {
        GlobalShortcuts::emit_bind_failed(self, failure);
    }
}

/// `Method`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Method {
    CreateSession,
    BindShortcuts,
}

impl Method {
    fn name(self) -> &'static str {
        match self {
            Method::CreateSession => "CreateSession",
            Method::BindShortcuts => "BindShortcuts",
        }
    }

    /// Construct the payload expected by the XDG portal call.
    fn make_payload(self, shortcuts: &GlobalShortcuts, request_token: &str) -> Option<glib::Variant> {
        match self {
            // See https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.GlobalShortcuts.html#org-freedesktop-portal-globalshortcuts-createsession
            Method::CreateSession => {
                // ({'handle_token': <%s>, 'session_handle_token': <%s>},)
                let options = glib::VariantDict::new(None);
                options.insert_value("handle_token", &request_token.to_variant());
                options.insert_value("session_handle_token", &generate_token().to_variant());
                Some(glib::Variant::tuple_from_iter([options.end()]))
            }

            // See https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.GlobalShortcuts.html#org-freedesktop-portal-globalshortcuts-bindshortcuts
            Method::BindShortcuts => {
                let priv_ = shortcuts.imp();
                let handle = priv_.handle.borrow().clone()?;

                let mut binds = Vec::new();
                for (trigger, action) in priv_.map.borrow().iter() {
                    let action = action.to_string();
                    if action.len() >= 256 {
                        continue;
                    }

                    // (%s, {'description': <%s>, 'preferred_trigger': <%s>})
                    let props = glib::VariantDict::new(None);
                    props.insert_value("description", &action.to_variant());
                    props.insert_value("preferred_trigger", &trigger.to_variant());
                    binds.push(glib::Variant::tuple_from_iter([trigger.to_variant(), props.end()]));
                }
                let binds = glib::Variant::array_from_iter_with_type(
                    glib::VariantTy::new("(sa{sv})").unwrap(),
                    binds,
                );

                // (%o, %*, '', {'handle_token': <%s>})
                let options = glib::VariantDict::new(None);
                options.insert_value("handle_token", &request_token.to_variant());
                let path = glib::variant::ObjectPath::try_from(handle).ok()?;
                Some(glib::Variant::tuple_from_iter([
                    path.to_variant(),
                    binds,
                    "".to_variant(),
                    options.end(),
                ]))
            }
        }
    }

    fn on_response(self, shortcuts: &GlobalShortcuts, vardict: &glib::Variant) {
        match self {
            Method::CreateSession => {
                let dict = glib::VariantDict::new(Some(vardict));
                let handle = match dict.lookup::<String>("session_handle") {
                    Ok(Some(h)) => h,
                    _ => {
                        log::warn!(target: LOG, "session handle not found in response={}", vardict.print(true));
                        return;
                    }
                };

                let priv_ = shortcuts.imp();
                let Some(dbus) = priv_.dbus_connection.borrow().clone() else { return };
                priv_.handle.replace(Some(handle.clone()));
                log::debug!(target: LOG, "session_handle={handle}");

                // Subscribe to keybind activations
                debug_assert!(priv_.activate_subscription.borrow().is_none());
                let weak = shortcuts.downgrade();
                let id = dbus.signal_subscribe(
                    None,
                    Some("org.freedesktop.portal.GlobalShortcuts"),
                    Some("Activated"),
                    Some("/org/freedesktop/portal/desktop"),
                    Some(&handle),
                    gio::DBusSignalFlags::MATCH_ARG0_PATH,
                    move |_, _, _, _, _, params| {
                        if let Some(this) = weak.upgrade() {
                            this.shortcut_activated(params);
                        }
                    },
                );
                priv_.activate_subscription.replace(Some(id));

                shortcuts.request(Method::BindShortcuts);
            }
            Method::BindShortcuts => {}
        }
    }
}

impl GlobalShortcuts {
    fn close(&self) {
        let priv_ = self.imp();

        // This is safe to call even if the winproto was already
        // deinitialized, which happens during application teardown. (The
        // object may also outlive the application, e.g. in tests.)
        if let Some(app) = Application::try_default() {
            app.with_winproto(|wp| wp.clear_global_shortcuts());
        }

        if let Some(dbus) = priv_.dbus_connection.borrow().as_ref() {
            if let Some(id) = priv_.response_subscription.take() {
                dbus.signal_unsubscribe(id);
            }

            if let Some(id) = priv_.activate_subscription.take() {
                dbus.signal_unsubscribe(id);
            }

            if let Some(handle) = priv_.handle.take() {
                // Close existing session
                dbus.call(
                    Some("org.freedesktop.portal.Desktop"),
                    &handle,
                    "org.freedesktop.portal.Session",
                    "Close",
                    None,
                    None,
                    gio::DBusCallFlags::NONE,
                    -1,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
            }
        }

        if priv_.arena.get() {
            priv_.arena.set(false);
            priv_.map.borrow_mut().clear(); // Uses arena memory
        }
    }

    fn refresh(&self) {
        // Always close our previous state first.
        self.close();

        let priv_ = self.imp();

        // We need a configuration to proceed.
        let Some(config) = priv_.config.borrow().clone() else { return };

        // Prefer a windowing protocol native mechanism over the
        // XDG desktop portal when available.
        let shortcuts: Rc<dyn winproto::GlobalShortcuts> = Rc::new(self.clone());
        let bound = Application::try_default()
            .and_then(|app| app.with_winproto(|wp| wp.bind_global_shortcuts(shortcuts, &config.get())))
            .unwrap_or(false);
        if bound {
            log::debug!(target: LOG, "global shortcuts bound via winproto");
            return;
        }

        // The portal fallback requires a dbus connection.
        if priv_.dbus_connection.borrow().is_none() {
            return;
        }

        // Setup our new arena that we'll use for memory allocations.
        debug_assert!(!priv_.arena.get());

        // Update map
        *priv_.map.borrow_mut() = build_map(&config.get());

        // Store our arena
        priv_.arena.set(true);

        // Create our session if we have global shortcuts.
        if !priv_.map.borrow().is_empty() {
            self.request(Method::CreateSession);
        }
    }

    /// Submit a request to the global shortcuts portal.
    fn request(&self, method: Method) {
        // NOTE(pluiedev):
        // XDG Portals are really, really poorly-designed pieces of hot garbage.
        // How the protocol is _initially_ designed to work is as follows:
        //
        // 1. The client calls a method which returns the path of a Request object;
        // 2. The client waits for the Response signal under said object path;
        // 3. When the signal arrives, the actual return value and status code
        //    become available for the client for further processing.
        //
        // THIS DOES NOT WORK. Once the first two steps are complete, the client
        // needs to immediately start listening for the third step, but an overeager
        // server implementation could easily send the Response signal before the
        // client is even ready, causing communications to break down over a simple
        // race condition/two generals' problem that even _TCP_ had figured out
        // decades ago. Worse yet, you get exactly _one_ chance to listen for the
        // signal, or else your communication attempt so far has all been in vain.
        //
        // And they know this. Instead of fixing their freaking protocol, they just
        // ask clients to manually construct the expected object path and subscribe
        // to the request signal beforehand, making the whole response value of
        // the original call COMPLETELY MEANINGLESS.
        //
        // Furthermore, this is _entirely undocumented_ aside from one tiny
        // paragraph under the documentation for the Request interface, and
        // anyone would be forgiven for missing it without reading the libportal
        // source code.
        //
        // When in Rome, do as the Romans do, I guess...?

        let request_token = generate_token();

        let Some(payload) = method.make_payload(self, &request_token) else { return };
        let request_path = self.get_request_path(&request_token);

        let priv_ = self.imp();
        let Some(dbus) = priv_.dbus_connection.borrow().clone() else { return };

        debug_assert!(priv_.response_subscription.borrow().is_none());
        let weak = self.downgrade();
        let id = dbus.signal_subscribe(
            None,
            Some("org.freedesktop.portal.Request"),
            Some("Response"),
            Some(&request_path),
            None,
            gio::DBusSignalFlags::NONE,
            move |dbus, _, _, _, _, params| {
                // See https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Request.html#org-freedesktop-portal-request-response
                let Some(this) = weak.upgrade() else { return };
                this.responded(dbus, method, params);
            },
        );
        priv_.response_subscription.replace(Some(id));

        dbus.call(
            Some("org.freedesktop.portal.Desktop"),
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.GlobalShortcuts",
            method.name(),
            Some(&payload),
            None,
            gio::DBusCallFlags::NONE,
            -1,
            None::<&gio::Cancellable>,
            |result| {
                // gotResponseHandle
                if let Err(err) = result {
                    log::warn!(target: LOG, "request failed={} ({})", err.message(), err.code());
                }

                // TODO: XDG recommends updating the signal subscription if the actual
                // returned request path is not the same as the expected request
                // path, to retain compatibility with older versions of XDG portals.
                // Although it suffers from the race condition outlined above,
                // we should still implement this at some point.
            },
        );
    }

    fn responded(&self, dbus: &gio::DBusConnection, method: Method, params: &glib::Variant) {
        let priv_ = self.imp();

        // Unsubscribe from the response signal
        if let Some(id) = priv_.response_subscription.take() {
            dbus.signal_unsubscribe(id);
        }

        // (u@a{sv})
        if params.n_children() < 2 {
            return;
        }
        let response: u32 = params.child_value(0).get().unwrap_or(u32::MAX);
        let vardict = params.child_value(1);

        match response {
            0 => {
                log::debug!(target: LOG, "request successful");
                method.on_response(self, &vardict);
            }
            1 => log::debug!(target: LOG, "request was cancelled by user"),
            2 => log::warn!(target: LOG, "request ended unexpectedly"),
            _ => log::warn!(target: LOG, "unrecognized response code={response}"),
        }
    }

    /// Get the XDG portal request path for the current Ghostty instance.
    ///
    /// If this sounds like nonsense, see `request` for an explanation as to
    /// why we need to do this.
    ///
    /// Precondition: dbus connection exists, arena setup
    fn get_request_path(&self, token: &str) -> String {
        let dbus = self.imp().dbus_connection.borrow().clone().unwrap();

        // `getUniqueName` should never return null here as we're using an ordinary
        // message bus connection. If it doesn't, something is very wrong
        let unique_name = dbus.unique_name().unwrap();
        request_path(&unique_name, token)
    }

    //---------------------------------------------------------------
    // Property Handlers

    pub fn set_dbus_connection(&self, dbus_connection: Option<&gio::DBusConnection>) {
        let priv_ = self.imp();

        // If we have a prior dbus connection we need to close our prior
        // registrations first.
        if priv_.dbus_connection.borrow().is_some() {
            self.close();
            priv_.dbus_connection.replace(None);
        }

        if let Some(v) = dbus_connection {
            priv_.dbus_connection.replace(Some(v.clone()));
            self.refresh();
        }

        self.notify(properties::DBUS_CONNECTION);
    }

    //---------------------------------------------------------------
    // Signal Handlers

    fn shortcut_activated(&self, params: &glib::Variant) {
        // 2nd value in the tuple is the activated shortcut ID
        // See https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.GlobalShortcuts.html#org-freedesktop-portal-globalshortcuts-activated
        if params.n_children() < 2 {
            return;
        }
        let shortcut_id = params.child_value(1);
        let Some(shortcut_id) = shortcut_id.str() else { return };
        log::debug!(target: LOG, "activated={shortcut_id}");

        let Some(action) = self.imp().map.borrow().get(shortcut_id).cloned() else { return };
        self.emit_trigger(&action);
    }

    /// Emit the trigger signal. Public for winproto shortcut backends.
    pub fn emit_trigger(&self, action: &Action) {
        self.emit_by_name::<()>(signals::TRIGGER, &[&class::pointer_value(action)]);
    }

    /// Emit the bind-failed signal. Public for winproto shortcut backends.
    pub fn emit_bind_failed(&self, failure: &BindFailed) {
        self.emit_by_name::<()>(signals::BIND_FAILED, &[failure]);
    }

    /// `signals.trigger.connect`
    pub fn connect_trigger<F: Fn(&Self, &Action) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_local(signals::TRIGGER, false, move |values| {
            let this: Self = values[0].get().unwrap();
            // SAFETY: the pointer is an `Action` that outlives the
            // synchronous emission.
            let action = unsafe { class::pointer_from_value::<Action>(&values[1]) };
            f(&this, action);
            None
        })
    }

    /// `signals.@"bind-failed".connect`
    pub fn connect_bind_failed<F: Fn(&Self, &BindFailed) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_local(signals::BIND_FAILED, false, move |values| {
            let this: Self = values[0].get().unwrap();
            let failure: BindFailed = values[1].get().unwrap();
            f(&this, &failure);
            None
        })
    }
}

/// The trigger map built by `refresh` from the global keybinds.
fn build_map(config: &CoreConfig) -> IndexMap<String, Action> {
    let mut map = IndexMap::new();

    let mut trigger_buf = [0u8; 1024];
    for (trigger_key, value) in config.keybind.set.bindings.iter() {
        let leaf: binding::GenericLeaf<'_> = match value {
            binding::Value::Leader(_) => continue,
            binding::Value::Leaf(leaf) => leaf.generic(),
            binding::Value::LeafChained(leaf) => leaf.generic(),
        };
        if !leaf.flags.global {
            continue;
        }

        // We only allow global keybinds that map to exactly a single
        // action for now. TODO: remove this restriction
        let actions = leaf.actions_slice();
        if actions.len() != 1 {
            continue;
        }

        let trigger = match key::xdg_shortcut_from_trigger(&mut trigger_buf, trigger_key.0) {
            Ok(Some(shortcut)) => shortcut.to_string_lossy().into_owned(),
            Ok(None) => continue,
            // If there isn't space to translate the trigger, then our
            // buffer might be too small (but 1024 is insane!). In any case
            // we don't want to stop registering globals.
            Err(_) => {
                log::warn!(
                    target: LOG,
                    "buffer too small to translate trigger, ignoring={}",
                    trigger_key.0
                );
                continue;
            }
        };

        map.insert(trigger, actions[0].clone());
    }

    map
}

/// The object path of a portal request (`getRequestPath` without the
/// connection).
fn request_path(unique_name: &str, token: &str) -> String {
    // See https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Request.html
    // for the syntax XDG portals expect.
    let object_path = [
        "/org/freedesktop/portal/desktop/request",
        &unique_name[1..], // Remove leading `:`
        token,
    ]
    .join("/");

    // Sanitize the unique name by replacing every `.` with `_`.
    // In effect, this will turn a unique name like `:1.192` into `1_192`.
    // Valid D-Bus object path components never contain `.`s anyway, so we're
    // free to replace all instances of `.` here and avoid extra allocation.
    object_path.replace('.', "_")
}

/// Generate a random token suitable for use in requests.
fn generate_token() -> String {
    // u28 takes up 7 bytes in hex, 8 bytes for "ghostty_" and 1 byte for NUL
    // 7 + 8 + 1 = 16
    let rand_int = getrandom::u32().unwrap_or(0) & 0x0FFF_FFFF;
    format!("ghostty_{rand_int:0<7x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::args::ParseCli;

    #[test]
    fn token_format() {
        for _ in 0..32 {
            let t = generate_token();
            assert_eq!(t.len(), 15);
            assert!(t.starts_with("ghostty_"));
            assert!(t[8..].chars().all(|c| c.is_ascii_hexdigit()));
        }
    }

    #[test]
    fn request_path_format() {
        assert_eq!(
            request_path(":1.192", "ghostty_abc0000"),
            "/org/freedesktop/portal/desktop/request/1_192/ghostty_abc0000"
        );
    }

    #[test]
    fn map_from_global_keybinds() {
        let mut config = CoreConfig::default();
        config
            .keybind
            .parse_cli(Some("global:ctrl+shift+a=new_window"))
            .unwrap();
        config.keybind.parse_cli(Some("ctrl+shift+b=new_tab")).unwrap();
        let map = build_map(&config);
        assert_eq!(map.len(), 1);
        let (trigger, action) = map.iter().next().unwrap();
        assert_eq!(trigger, "SHIFT+CTRL+a");
        assert_eq!(*action, Action::NewWindow);
    }

    #[test]
    fn payloads() {
        let s = GlobalShortcuts::default();
        let create = Method::CreateSession.make_payload(&s, "ghostty_1234567").unwrap();
        assert_eq!(create.type_().as_str(), "(a{sv})");

        // No session handle yet.
        assert!(Method::BindShortcuts.make_payload(&s, "t").is_none());
        s.imp().handle.replace(Some("/org/freedesktop/portal/desktop/session/1_2/x".into()));
        s.imp().map.borrow_mut().insert("CTRL+a".into(), Action::NewWindow);
        let bind = Method::BindShortcuts.make_payload(&s, "t").unwrap();
        assert_eq!(bind.type_().as_str(), "(oa(sa{sv})sa{sv})");
        assert_eq!(bind.child_value(1).n_children(), 1);
    }

    #[test]
    fn signals_and_activation() {
        let s = GlobalShortcuts::default();
        let got = Rc::new(RefCell::new(Vec::new()));
        let g = got.clone();
        s.connect_trigger(move |_, a| g.borrow_mut().push(a.clone()));
        s.imp().map.borrow_mut().insert("CTRL+a".into(), Action::NewTab);

        let params = glib::Variant::tuple_from_iter([
            "/session".to_variant(),
            "CTRL+a".to_variant(),
            0u64.to_variant(),
            glib::VariantDict::new(None).end(),
        ]);
        s.shortcut_activated(&params);
        let unknown = glib::Variant::tuple_from_iter(["/s".to_variant(), "nope".to_variant()]);
        s.shortcut_activated(&unknown);
        assert_eq!(*got.borrow(), vec![Action::NewTab]);

        let failed = Rc::new(Cell::new(false));
        let f = failed.clone();
        s.connect_bind_failed(move |_, b| f.set(b.revoked));
        winproto::GlobalShortcuts::emit_bind_failed(
            &s,
            &BindFailed {
                trigger: Trigger::default(),
                action: Action::NewTab,
                message: String::new(),
                revoked: true,
            },
        );
        assert!(failed.get());

        // Without winproto or D-Bus, a config change sets up nothing.
        s.set_property(properties::CONFIG, Config::new(&CoreConfig::default()));
        assert!(!s.imp().arena.get());
    }
}
