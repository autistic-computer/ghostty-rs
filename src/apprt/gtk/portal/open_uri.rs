//! Port of `src/apprt/gtk/portal/OpenURI.zig`.
//!
//! STATUS: ported
//!
//! Use DBus to call the XDG Desktop Portal to open an URI.
//! See: https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.OpenURI.html#org-freedesktop-portal-openuri-openuri
//!
//! Zig keeps the state in the `OpenURI` struct embedded in `App` and passes
//! `*OpenURI`/`*RequestData` as C user data to the D-Bus and timer
//! callbacks. The port keeps the state in an `Rc` shared by the closures
//! (all callbacks run on the main context). Functions that Zig documents as
//! "must be called with the mutex locked" (asserted with `tryLock`) take the
//! locked `Inner` instead, which the compiler enforces.

use std::rc::{Rc, Weak};
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

use gio::prelude::*;
use gtk4::gdk::prelude::*;
use indexmap::IndexMap;

use super as portal;
pub use crate::apprt::action::{OpenUrl, OpenUrlKind};

/// `self.app.app.openUrlFallback(kind, url)`: the GTK `Application`'s
/// fallback URL opener.
// NOTE(port): Zig keeps `app: *App`; here the `Application` owns this state,
// so it passes a closure calling `Application::open_url_fallback` instead of
// a back-reference (tests pass recording closures).
pub type OpenUrlFallback = Box<dyn Fn(OpenUrlKind, &str)>;

const LOG: &str = "openuri";

struct RequestData {
    token: usize,
    kind: OpenUrlKind,
    uri: String,
    request_path: String,
}

/// Data about any in-flight calls to the portal.
pub struct Entry {
    /// When the request started.
    pub start: Instant,
    /// A token used by the portal to identify requests and responses. The
    /// actual format of the token does not really matter as long as it can be
    /// used as part of a D-Bus object path. `usize` was chosen since it's easy
    /// to hash and to generate random tokens.
    pub token: usize,
    /// The "kind" of URI. Unused here, but we may need to pass it on to the
    /// fallback URL opener if the D-Bus method fails.
    pub kind: OpenUrlKind,
    /// A copy of the URI that we are opening. We need our own copy since the
    /// method calls are asynchronous and the original may have been freed by
    /// the time we need it.
    pub uri: String,
    /// An xdg-activation token for the input that opened the URI, passed to
    /// the portal so the app it launches can raise its window.
    pub activation_token: Option<String>,
    /// Used to manage a subscription to a D-Bus signal, which is how the XDG
    /// Portal reports results of the method call.
    subscription: Option<gio::SignalSubscriptionId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Errors {
    /// Could not get a D-Bus connection
    DBusConnectionRequired,
    /// The D-Bus connection did not have a unique name. This _should_ be
    /// impossible, but is handled for safety's sake.
    NoDBusUniqueName,
    /// The system was unable to give us the time.
    TimerUnavailable,
}

impl std::fmt::Display for Errors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for Errors {}

struct Inner {
    /// Connection to the D-Bus session bus that we'll use for all of our messaging.
    dbus: Option<gio::DBusConnection>,

    /// Map to store data about any in-flight calls to the portal.
    entries: IndexMap<usize, Entry>,

    /// Used to manage a timer to clean up any orphan entries in the map.
    cleanup_timer: Option<glib::SourceId>,

    /// Set to false during shutdown so callbacks stop touching internal state.
    alive: bool,
}

struct Shared {
    /// The GTK app that we "belong" to (its URL fallback).
    open_url_fallback: OpenUrlFallback,

    /// Mutex to protect modification of the entries map or the cleanup timer.
    mutex: Mutex<Inner>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.mutex.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub struct OpenUri {
    shared: Rc<Shared>,
}

impl OpenUri {
    pub fn init(open_url_fallback: OpenUrlFallback) -> OpenUri {
        OpenUri {
            shared: Rc::new(Shared {
                open_url_fallback,
                mutex: Mutex::new(Inner {
                    dbus: None,
                    entries: IndexMap::new(),
                    cleanup_timer: None,
                    alive: true,
                }),
            }),
        }
    }

    pub fn set_dbus_connection(&self, dbus: Option<gio::DBusConnection>) {
        self.shared.lock().dbus = dbus;
    }

    pub fn deinit(&self) {
        let mut inner = self.shared.lock();

        if !inner.alive {
            return;
        }
        inner.alive = false;

        stop_cleanup_timer(&mut inner);

        let entries: Vec<Entry> = inner.entries.drain(..).map(|(_, e)| e).collect();
        for mut entry in entries {
            unsubscribe_from_response(inner.dbus.as_ref(), &mut entry);
        }

        inner.dbus = None;
    }

    /// Send the D-Bus method call to the XDG Desktop portal. The result of the
    /// method call will be reported asynchronously.
    pub fn start(&self, value: OpenUrl<'_>) -> Result<(), Errors> {
        let dbus = self.shared.lock().dbus.clone().ok_or(Errors::DBusConnectionRequired)?;

        let token = portal::generate_token();
        let request_path =
            portal::get_request_path(&dbus, token).map_err(|_| Errors::NoDBusUniqueName)?;

        // The URL is passed to D-Bus as a string (`s`), which must be UTF-8.
        let url = String::from_utf8_lossy(value.url).into_owned();

        let request = RequestData {
            token,
            kind: value.kind,
            uri: url.clone(),
            request_path: request_path.clone(),
        };

        // Requested before taking the lock: it waits on a Wayland roundtrip.
        let activation_token = activation_token();

        let mut inner = self.shared.lock();

        // Create an entry that is used to track the results of the D-Bus method
        // call.
        let entry = Entry {
            start: Instant::now(),
            token,
            kind: value.kind,
            uri: url,
            activation_token,
            subscription: None,
        };
        debug_assert!(!inner.entries.contains_key(&token));
        inner.entries.insert(token, entry);

        start_cleanup_timer(&self.shared, &mut inner);
        subscribe_to_response(&self.shared, &mut inner, token, &dbus, &request_path);
        send_request(&self.shared, &inner, token, &dbus, request);
        Ok(())
    }
}

impl Drop for OpenUri {
    fn drop(&mut self) {
        self.deinit();
    }
}

/// Ask GDK for an xdg-activation token tied to the most recent input, which is
/// what opened the URI. Returns null if the platform can't provide one.
fn activation_token() -> Option<String> {
    let display = gtk4::gdk::Display::default()?;
    let context = display.app_launch_context();

    let token = context.startup_notify_id(None::<&gio::AppInfo>, &[])?;

    Some(token.to_string())
}

/// Subscribe to the D-Bus signal that will contain the results of our method
/// call to the portal. This must be called with the mutex locked.
#[allow(deprecated)]
fn subscribe_to_response(
    shared: &Rc<Shared>,
    inner: &mut Inner,
    token: usize,
    dbus: &gio::DBusConnection,
    request_path: &str,
) {
    let Some(entry) = inner.entries.get_mut(&token) else { return };
    if entry.subscription.is_some() {
        return;
    }

    let weak = Rc::downgrade(shared);
    entry.subscription = Some(dbus.signal_subscribe(
        None,
        Some("org.freedesktop.portal.Request"),
        Some("Response"),
        Some(request_path),
        None,
        gio::DBusSignalFlags::NONE,
        move |_, _, object_path, _, _, params| {
            response_received(&weak, object_path, params);
        },
    ));
}

/// Unsubscribe to the D-Bus signal that contains the result of the method call.
/// This will prevent a response from being processed multiple times. This must
/// be called when the mutex is locked.
#[allow(deprecated)]
fn unsubscribe_from_response(dbus: Option<&gio::DBusConnection>, entry: &mut Entry) {
    // Unsubscribe from the response signal
    if let Some(subscription) = entry.subscription.take() {
        let Some(dbus) = dbus else {
            log::warn!(
                target: LOG,
                "unable to unsubscribe open uri response without dbus connection"
            );
            return;
        };
        dbus.signal_unsubscribe(subscription);
    }
}

fn fail_request(shared: &Shared, token: usize) -> Option<Entry> {
    let mut inner = shared.lock();

    if !inner.alive {
        return None;
    }

    let mut entry = inner.entries.swap_remove(&token)?;
    unsubscribe_from_response(inner.dbus.as_ref(), &mut entry);
    Some(entry)
}

fn fail_request_and_fallback(shared: &Shared, request: &RequestData) {
    let Some(_entry) = fail_request(shared, request.token) else { return };

    (shared.open_url_fallback)(request.kind, &request.uri);
}

/// Send the D-Bus method call to the portal. The mutex must be locked when this
/// is called.
fn send_request(
    shared: &Rc<Shared>,
    inner: &Inner,
    token: usize,
    dbus: &gio::DBusConnection,
    request: RequestData,
) {
    let Some(entry) = inner.entries.get(&token) else { return };

    let payload = {
        let option = |key: &str, value: glib::Variant| {
            glib::variant::DictEntry::new(key.to_owned(), value).to_variant()
        };

        // Options
        let mut options = vec![
            option("handle_token", portal::format_token(entry.token).to_variant()),
            option("writable", false.to_variant()),
            option("ask", false.to_variant()),
        ];
        if let Some(token) = &entry.activation_token {
            options.push(option("activation_token", token.to_variant()));
        }
        let options =
            glib::Variant::array_from_iter_with_type(glib::VariantTy::new("{sv}").unwrap(), options);

        glib::Variant::tuple_from_iter([
            // parent window - empty string means we have no window
            "".to_variant(),
            // URI to open
            entry.uri.to_variant(),
            options,
        ])
    };

    // We're expecting an object path back from the method call.
    let reply_type = glib::VariantTy::new("(o)").unwrap();

    let weak = Rc::downgrade(shared);
    let source = dbus.clone();
    dbus.call(
        Some("org.freedesktop.portal.Desktop"),
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.OpenURI",
        "OpenURI",
        Some(&payload),
        Some(reply_type),
        gio::DBusCallFlags::NONE,
        -1,
        None::<&gio::Cancellable>,
        move |result| request_callback(&weak, &source, result, request),
    );
}

/// Process the result of the original method call. Receiving this result does
/// not indicate that the that the method call succeeded but it may contain an
/// error message that is useful to log for debugging purposes.
fn request_callback(
    weak: &Weak<Shared>,
    dbus: &gio::DBusConnection,
    result: Result<glib::Variant, glib::Error>,
    request: RequestData,
) {
    let Some(shared) = weak.upgrade() else { return };

    let reply = match result {
        Err(err) => {
            log::error!(
                target: LOG,
                "Open URI request failed={} ({})",
                err.message(),
                err.code(),
            );
            fail_request_and_fallback(&shared, &request);
            return;
        }
        Ok(reply) => reply,
    };

    if !reply.is_type(glib::VariantTy::new("(o)").unwrap()) {
        log::warn!(
            target: LOG,
            "Reply from D-Bus method call does not contain an object path!"
        );
        fail_request_and_fallback(&shared, &request);
        return;
    }

    let object_path: String = reply.child_value(0).str().unwrap_or_default().to_owned();

    let Some(token) = portal::parse_request_path_token(&object_path) else {
        log::warn!(target: LOG, "Unable to parse token from the object path {object_path}");
        fail_request_and_fallback(&shared, &request);
        return;
    };

    if token != request.token {
        log::warn!(
            target: LOG,
            "Open URI request returned mismatched token expected={:x} actual={:x}",
            request.token,
            token,
        );
        fail_request_and_fallback(&shared, &request);
        return;
    }

    let mut inner = shared.lock();

    if !inner.alive {
        return;
    }

    if !inner.entries.contains_key(&token) {
        return;
    }
    if request.request_path == object_path {
        return;
    }

    log::debug!(
        target: LOG,
        "updating open uri request path old={} new={}",
        request.request_path,
        object_path,
    );
    {
        let Inner { dbus, entries, .. } = &mut *inner;
        let entry = entries.get_mut(&token).unwrap();
        unsubscribe_from_response(dbus.as_ref(), entry);
    }
    subscribe_to_response(&shared, &mut inner, token, dbus, &object_path);
}

/// Handle the response signal from the portal. This should contain the actual
/// results of the method call (success or failure).
fn response_received(weak: &Weak<Shared>, object_path: &str, params: &glib::Variant) {
    let Some(shared) = weak.upgrade() else {
        log::error!(target: LOG, "OpenURI response received with null userdata");
        return;
    };

    let Some(token) = portal::parse_request_path_token(object_path) else {
        log::warn!(target: LOG, "invalid object path: {object_path}");
        return;
    };

    let mut inner = shared.lock();

    if !inner.alive {
        return;
    }

    let Some(mut entry) = inner.entries.swap_remove(&token) else {
        log::warn!(target: LOG, "no entry for token {token:x}");
        return;
    };

    unsubscribe_from_response(inner.dbus.as_ref(), &mut entry);

    let response: u32 = if params.n_children() > 0 {
        params.child_value(0).get::<u32>().unwrap_or(u32::MAX)
    } else {
        u32::MAX
    };

    match response {
        0 => {
            log::debug!(target: LOG, "open uri successful");
        }
        1 => {
            log::debug!(target: LOG, "open uri request was cancelled by the user");
        }
        2 => {
            log::warn!(target: LOG, "open uri request ended unexpectedly");
            (shared.open_url_fallback)(entry.kind, &entry.uri);
        }
        _ => {
            log::error!(target: LOG, "unrecognized response code={response}");
            (shared.open_url_fallback)(entry.kind, &entry.uri);
        }
    }
}

/// Wait this number of seconds and then clean up any orphaned entries.
const CLEANUP_TIMEOUT: u64 = 30;

/// If there is an active cleanup timer, cancel it. This must be called with the
/// mutex locked
fn stop_cleanup_timer(inner: &mut Inner) {
    if let Some(timer) = inner.cleanup_timer.take() {
        let raw = timer.as_raw();
        if !crate::apprt::gtk::ext::source_remove(timer) {
            log::warn!(target: LOG, "unable to remove cleanup timer source={raw}");
        }
    }
}

/// Start a timer to clean up any entries that have not received a timely
/// response. If there is already a timer it will be stopped and replaced with a
/// new one. This must be called with the mutex locked.
fn start_cleanup_timer(shared: &Rc<Shared>, inner: &mut Inner) {
    stop_cleanup_timer(inner);
    let weak = Rc::downgrade(shared);
    inner.cleanup_timer = Some(glib::timeout_add_seconds_local(
        (CLEANUP_TIMEOUT + 1) as u32,
        move || {
            cleanup(&weak);
            glib::ControlFlow::Break
        },
    ));
}

/// The cleanup timer is used to free up any entries that may have failed
/// to get a response in a timely manner.
fn cleanup(weak: &Weak<Shared>) {
    let Some(shared) = weak.upgrade() else {
        log::warn!(target: LOG, "cleanup called with null userdata");
        return;
    };

    let mut inner = shared.lock();

    // The source is being removed by returning `Break`; forget its ID so it
    // is not removed twice.
    inner.cleanup_timer = None;
    if !inner.alive {
        return;
    }

    loop {
        let expired = inner
            .entries
            .values()
            .find(|entry| entry.start.elapsed().as_secs() > CLEANUP_TIMEOUT)
            .map(|entry| entry.token);
        let Some(token) = expired else { break };

        log::warn!(target: LOG, "open uri request timed out token={token:x}");
        let mut entry = inner.entries.swap_remove(&token).unwrap();
        unsubscribe_from_response(inner.dbus.as_ref(), &mut entry);
        (shared.open_url_fallback)(entry.kind, &entry.uri);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn start_requires_dbus() {
        let open_uri = OpenUri::init(Box::new(|_, _| {}));
        assert_eq!(
            open_uri.start(OpenUrl {
                kind: OpenUrlKind::Text,
                url: b"https://ghostty.org",
            }),
            Err(Errors::DBusConnectionRequired)
        );
        open_uri.deinit();
        open_uri.deinit();
    }

    #[test]
    fn failed_request_falls_back() {
        let calls: Rc<RefCell<Vec<(OpenUrlKind, String)>>> = Rc::default();
        let c = calls.clone();
        let open_uri = OpenUri::init(Box::new(move |kind, url| {
            c.borrow_mut().push((kind, url.to_owned()))
        }));
        {
            let mut inner = open_uri.shared.lock();
            inner.entries.insert(
                7,
                Entry {
                    start: Instant::now(),
                    token: 7,
                    kind: OpenUrlKind::Html,
                    uri: "file:///x.html".to_owned(),
                    activation_token: None,
                    subscription: None,
                },
            );
        }
        let request = RequestData {
            token: 7,
            kind: OpenUrlKind::Html,
            uri: "file:///x.html".to_owned(),
            request_path: String::new(),
        };
        fail_request_and_fallback(&open_uri.shared, &request);
        assert_eq!(&*calls.borrow(), &[(OpenUrlKind::Html, "file:///x.html".to_owned())]);
        assert!(open_uri.shared.lock().entries.is_empty());

        // A second failure for the same token is a no-op.
        fail_request_and_fallback(&open_uri.shared, &request);
        assert_eq!(calls.borrow().len(), 1);
    }

    #[test]
    fn response_codes() {
        let calls: Rc<RefCell<Vec<String>>> = Rc::default();
        let c = calls.clone();
        let open_uri = OpenUri::init(Box::new(move |_, url| c.borrow_mut().push(url.to_owned())));
        for (token, code) in [(1usize, 0u32), (2, 1), (3, 2), (4, 9)] {
            open_uri.shared.lock().entries.insert(
                token,
                Entry {
                    start: Instant::now(),
                    token,
                    kind: OpenUrlKind::Unknown,
                    uri: format!("u{code}"),
                    activation_token: None,
                    subscription: None,
                },
            );
            let params = glib::Variant::tuple_from_iter([
                code.to_variant(),
                glib::VariantDict::new(None).end(),
            ]);
            let path = format!("/org/freedesktop/portal/desktop/request/1_2/{}", portal::format_token(token));
            response_received(&Rc::downgrade(&open_uri.shared), &path, &params);
        }
        assert_eq!(&*calls.borrow(), &["u2".to_owned(), "u9".to_owned()]);
        assert!(open_uri.shared.lock().entries.is_empty());
    }
}
