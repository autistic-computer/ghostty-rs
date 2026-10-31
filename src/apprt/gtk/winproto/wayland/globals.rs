//! Port of `src/apprt/gtk/winproto/wayland/Globals.zig`.
//!
//! STATUS: ported
//!
//! Zig binds the globals with zig-wayland on libwayland's default queue,
//! which GDK dispatches, and installs C listeners with a `*Globals` as
//! user data. The port uses the `wayland-client` crate on GDK's
//! `wl_display`: our proxies live on their own [`EventQueue`] whose state
//! is [`Globals`] (the `Dispatch` impls below are the listeners), and
//! [`Shared`] owns that queue plus a GLib main-loop source that dispatches
//! it whenever GDK has read events for it from the shared socket.
//! Hotkey events are not handled during dispatch (their handlers emit GTK
//! signals, which may re-enter the winproto code); they are queued in
//! [`Globals::hotkey_events`] and handed to the registered hotkey sink once
//! dispatching is done.

use std::cell::{Cell, RefCell};
use std::os::raw::c_int;
use std::rc::{Rc, Weak};

use wayland_client::protocol::{wl_compositor, wl_display, wl_region, wl_registry};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};
use wayland_protocols::ext::background_effect::v1::client::{
    ext_background_effect_manager_v1, ext_background_effect_surface_v1,
};
use wayland_protocols::xdg::activation::v1::client::{xdg_activation_token_v1, xdg_activation_v1};
use wayland_protocols_plasma::output_order::v1::client::kde_output_order_v1;
use wayland_protocols_plasma::server_decoration::client::{
    org_kde_kwin_server_decoration, org_kde_kwin_server_decoration_manager,
};
use wayland_protocols_plasma::slide::client::{org_kde_kwin_slide, org_kde_kwin_slide_manager};

use super::super::Error;

const LOG: &str = "winproto_wayland_globals";

/// The vicinae-hotkey protocol (`wayland.client.vicinae`), generated from
/// `protocols/vicinae-hotkey-v1.xml`.
pub mod vicinae {
    #![allow(missing_docs, non_upper_case_globals, non_camel_case_types, clippy::all)]
    use wayland_client;
    use wayland_client::protocol::*;

    pub mod __interfaces {
        use wayland_client::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!(
            "src/apprt/gtk/winproto/wayland/protocols/vicinae-hotkey-v1.xml"
        );
    }
    use self::__interfaces::*;

    wayland_scanner::generate_client_code!(
        "src/apprt/gtk/winproto/wayland/protocols/vicinae-hotkey-v1.xml"
    );
}

use self::vicinae::{vicinae_hotkey_manager_v1, vicinae_hotkey_v1};

pub type DecorationMode = org_kde_kwin_server_decoration_manager::Mode;
pub type BgEffectCapability = ext_background_effect_manager_v1::Capability;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tag {
    Compositor,
    ExtBackgroundEffect,
    KdeDecorationManager,
    KdeSlideManager,
    KdeOutputOrder,
    VicinaeHotkeyManager,
    XdgActivation,
}

impl Tag {
    const ALL: [Tag; 7] = [
        Tag::Compositor,
        Tag::ExtBackgroundEffect,
        Tag::KdeDecorationManager,
        Tag::KdeSlideManager,
        Tag::KdeOutputOrder,
        Tag::VicinaeHotkeyManager,
        Tag::XdgActivation,
    ];

    fn index(self) -> usize {
        self as usize
    }

    /// `T.interface.name` of `Tag.Type()`.
    fn interface_name(self) -> &'static str {
        match self {
            Tag::Compositor => wl_compositor::WlCompositor::interface().name,
            Tag::ExtBackgroundEffect => {
                ext_background_effect_manager_v1::ExtBackgroundEffectManagerV1::interface().name
            }
            Tag::KdeDecorationManager => {
                org_kde_kwin_server_decoration_manager::OrgKdeKwinServerDecorationManager::interface()
                    .name
            }
            Tag::KdeSlideManager => org_kde_kwin_slide_manager::OrgKdeKwinSlideManager::interface().name,
            Tag::KdeOutputOrder => kde_output_order_v1::KdeOutputOrderV1::interface().name,
            Tag::VicinaeHotkeyManager => vicinae_hotkey_manager_v1::VicinaeHotkeyManagerV1::interface().name,
            Tag::XdgActivation => xdg_activation_v1::XdgActivationV1::interface().name,
        }
    }
}

/// A bound global (`Binding.proxy`, typed).
#[derive(Debug, Clone)]
pub enum BoundProxy {
    Compositor(wl_compositor::WlCompositor),
    ExtBackgroundEffect(ext_background_effect_manager_v1::ExtBackgroundEffectManagerV1),
    KdeDecorationManager(org_kde_kwin_server_decoration_manager::OrgKdeKwinServerDecorationManager),
    KdeSlideManager(org_kde_kwin_slide_manager::OrgKdeKwinSlideManager),
    KdeOutputOrder(kde_output_order_v1::KdeOutputOrderV1),
    VicinaeHotkeyManager(vicinae_hotkey_manager_v1::VicinaeHotkeyManagerV1),
    XdgActivation(xdg_activation_v1::XdgActivationV1),
}

impl BoundProxy {
    fn id(&self) -> wayland_client::backend::ObjectId {
        match self {
            BoundProxy::Compositor(p) => p.id(),
            BoundProxy::ExtBackgroundEffect(p) => p.id(),
            BoundProxy::KdeDecorationManager(p) => p.id(),
            BoundProxy::KdeSlideManager(p) => p.id(),
            BoundProxy::KdeOutputOrder(p) => p.id(),
            BoundProxy::VicinaeHotkeyManager(p) => p.id(),
            BoundProxy::XdgActivation(p) => p.id(),
        }
    }
}

/// `Tag.Type()`: the proxy type of each global.
pub trait Global: Proxy + Clone {
    const TAG: Tag;
    fn from_bound(bound: &BoundProxy) -> Option<&Self>;
}

macro_rules! global {
    ($ty:ty, $tag:ident) => {
        impl Global for $ty {
            const TAG: Tag = Tag::$tag;
            fn from_bound(bound: &BoundProxy) -> Option<&Self> {
                match bound {
                    BoundProxy::$tag(p) => Some(p),
                    _ => None,
                }
            }
        }
    };
}

global!(wl_compositor::WlCompositor, Compositor);
global!(ext_background_effect_manager_v1::ExtBackgroundEffectManagerV1, ExtBackgroundEffect);
global!(
    org_kde_kwin_server_decoration_manager::OrgKdeKwinServerDecorationManager,
    KdeDecorationManager
);
global!(org_kde_kwin_slide_manager::OrgKdeKwinSlideManager, KdeSlideManager);
global!(kde_output_order_v1::KdeOutputOrderV1, KdeOutputOrder);
global!(vicinae_hotkey_manager_v1::VicinaeHotkeyManagerV1, VicinaeHotkeyManager);
global!(xdg_activation_v1::XdgActivationV1, XdgActivation);

#[derive(Debug)]
struct Binding {
    // All globals can be casted into a wl.Proxy object.
    proxy: BoundProxy,
    name: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    /// Connector name of the primary output (e.g., "DP-1") as reported
    /// by kde_output_order_v1. The first output in each priority list
    /// is the primary.
    pub primary_output_name: Option<String>,

    /// Tracks the output order event cycle. Set to true after a `done`
    /// event so the next `output` event is captured as the new primary.
    /// Initialized to true so the first event after binding is captured.
    pub output_order_done: bool,

    pub default_deco_mode: Option<DecorationMode>,

    pub bg_effect_capabilities: BgEffectCapability,
}

impl Default for State {
    fn default() -> Self {
        State {
            primary_output_name: None,
            output_order_done: true,
            default_deco_mode: None,
            bg_effect_capabilities: BgEffectCapability::empty(),
        }
    }
}

impl State {
    /// Reset cached state derived from kde_output_order_v1.
    fn reset_output_order(&mut self) {
        self.primary_output_name = None;
        self.output_order_done = true;
    }
}

/// User data of an xdg-activation token: the surface to activate.
#[derive(Debug, Clone)]
pub struct ActivationTokenData {
    pub surface: wayland_client::protocol::wl_surface::WlSurface,
}

/// User data of a vicinae hotkey: the `Hotkeys` entry it belongs to.
#[derive(Debug, Clone, Copy)]
pub struct HotkeyData {
    pub id: u64,
}

pub struct Globals {
    pub state: State,
    map: [Option<Binding>; 7],

    /// Used in the initial roundtrip to determine whether more
    /// roundtrips are required to fetch the initial state.
    needs_roundtrip: bool,

    /// Hotkey events received during the current dispatch (see the module
    /// docs).
    pub(crate) hotkey_events: Vec<(u64, vicinae_hotkey_v1::Event)>,
}

impl Globals {
    fn new() -> Globals {
        Globals {
            state: State::default(),
            map: Default::default(),
            needs_roundtrip: false,
            hotkey_events: Vec::new(),
        }
    }

    pub fn init(display: &wl_display::WlDisplay) -> Result<Rc<Shared>, Error> {
        Shared::init(display)
    }

    pub fn get<T: Global>(&self) -> Option<T> {
        let binding = self.map[T::TAG.index()].as_ref()?;
        T::from_bound(&binding.proxy).cloned()
    }

    fn on_global_attached(&mut self, tag: Tag) {
        // Install listeners immediately at bind time. This
        // keeps listener setup and object lifetime in one
        // place and also supports globals that appear later.
        //
        // (The listeners are the `Dispatch` impls; only the roundtrip
        // bookkeeping is left here.)
        match tag {
            Tag::ExtBackgroundEffect | Tag::KdeDecorationManager | Tag::KdeOutputOrder => {
                self.needs_roundtrip = true;
            }
            _ => {}
        }
    }

    fn on_global_removed(&mut self, tag: Tag) {
        if tag == Tag::KdeOutputOrder {
            self.state.reset_output_order();
        }
    }

    /// `wl_proxy_destroy`: destroy the client-side object without sending
    /// a request (what Zig's `proxy.destroy()` does).
    fn destroy_proxy(proxy: &BoundProxy) {
        let id = proxy.id();
        let backend = match proxy {
            BoundProxy::Compositor(p) => p.backend().upgrade(),
            BoundProxy::ExtBackgroundEffect(p) => p.backend().upgrade(),
            BoundProxy::KdeDecorationManager(p) => p.backend().upgrade(),
            BoundProxy::KdeSlideManager(p) => p.backend().upgrade(),
            BoundProxy::KdeOutputOrder(p) => p.backend().upgrade(),
            BoundProxy::VicinaeHotkeyManager(p) => p.backend().upgrade(),
            BoundProxy::XdgActivation(p) => p.backend().upgrade(),
        };
        if let Some(backend) = backend {
            let _ = backend.destroy_object(&id);
        }
    }

    fn bind(
        registry: &wl_registry::WlRegistry,
        tag: Tag,
        name: u32,
        qh: &QueueHandle<Globals>,
    ) -> BoundProxy {
        // zig-wayland binds every global at the version it was generated
        // for, which is 1 for all of them (`scanner.generate(..., 1)`).
        const GENERATED_VERSION: u32 = 1;
        match tag {
            Tag::Compositor => BoundProxy::Compositor(registry.bind(name, GENERATED_VERSION, qh, ())),
            Tag::ExtBackgroundEffect => {
                BoundProxy::ExtBackgroundEffect(registry.bind(name, GENERATED_VERSION, qh, ()))
            }
            Tag::KdeDecorationManager => {
                BoundProxy::KdeDecorationManager(registry.bind(name, GENERATED_VERSION, qh, ()))
            }
            Tag::KdeSlideManager => BoundProxy::KdeSlideManager(registry.bind(name, GENERATED_VERSION, qh, ())),
            Tag::KdeOutputOrder => BoundProxy::KdeOutputOrder(registry.bind(name, GENERATED_VERSION, qh, ())),
            Tag::VicinaeHotkeyManager => {
                BoundProxy::VicinaeHotkeyManager(registry.bind(name, GENERATED_VERSION, qh, ()))
            }
            Tag::XdgActivation => BoundProxy::XdgActivation(registry.bind(name, GENERATED_VERSION, qh, ())),
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for Globals {
    fn event(
        self_: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global { name, interface, .. } => {
                log::debug!(target: LOG, "found global {interface}");
                for tag in Tag::ALL {
                    if interface != tag.interface_name() {
                        continue;
                    }
                    log::debug!(target: LOG, "matched {tag:?}");

                    let new_proxy = Globals::bind(registry, tag, name, qh);

                    // If this global was already bound,
                    // then we also need to destroy the old binding.
                    if let Some(old) = self_.map[tag.index()].take() {
                        self_.on_global_removed(tag);
                        Globals::destroy_proxy(&old.proxy);
                    }

                    self_.map[tag.index()] = Some(Binding {
                        proxy: new_proxy,
                        name,
                    });
                    self_.on_global_attached(tag);
                }
            }

            // This should be a rare occurrence, but in case a global
            // is suddenly no longer available, we destroy and unset it
            // as the protocol mandates.
            wl_registry::Event::GlobalRemove { name } => {
                for tag in Tag::ALL {
                    let matches = self_.map[tag.index()]
                        .as_ref()
                        .is_some_and(|b| b.name == name);
                    if !matches {
                        continue;
                    }
                    self_.on_global_removed(tag);
                    if let Some(binding) = self_.map[tag.index()].take() {
                        Globals::destroy_proxy(&binding.proxy);
                    }
                }
            }

            _ => {}
        }
    }
}

impl Dispatch<ext_background_effect_manager_v1::ExtBackgroundEffectManagerV1, ()> for Globals {
    fn event(
        self_: &mut Self,
        _: &ext_background_effect_manager_v1::ExtBackgroundEffectManagerV1,
        event: ext_background_effect_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_background_effect_manager_v1::Event::Capabilities { flags } = event {
            self_.state.bg_effect_capabilities = match flags {
                WEnum::Value(v) => v,
                WEnum::Unknown(v) => BgEffectCapability::from_bits_truncate(v),
            };
        }
    }
}

impl Dispatch<org_kde_kwin_server_decoration_manager::OrgKdeKwinServerDecorationManager, ()> for Globals {
    fn event(
        self_: &mut Self,
        _: &org_kde_kwin_server_decoration_manager::OrgKdeKwinServerDecorationManager,
        event: org_kde_kwin_server_decoration_manager::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let org_kde_kwin_server_decoration_manager::Event::DefaultMode { mode } = event {
            // (Zig's `@fromBackingInt` would trap on an unknown mode.)
            self_.state.default_deco_mode = DecorationMode::try_from(mode).ok();
        }
    }
}

impl Dispatch<kde_output_order_v1::KdeOutputOrderV1, ()> for Globals {
    fn event(
        self_: &mut Self,
        _: &kde_output_order_v1::KdeOutputOrderV1,
        event: kde_output_order_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        output_order_event(&mut self_.state, event);
    }
}

fn output_order_event(state: &mut State, event: kde_output_order_v1::Event) {
    match event {
        kde_output_order_v1::Event::Output { output_name } => {
            // Only the first output event after a `done` is the new primary.
            if !state.output_order_done {
                return;
            }
            state.output_order_done = false;

            let name = output_name.split('\0').next().unwrap_or("");

            if name.is_empty() {
                state.primary_output_name = None;
                log::warn!(
                    target: LOG,
                    "ignoring empty primary output name from kde_output_order_v1"
                );
            } else {
                state.primary_output_name = Some(name.to_owned());
                log::debug!(target: LOG, "primary output: {name}");
            }
        }
        kde_output_order_v1::Event::Done => {
            if state.output_order_done {
                // No output arrived since the previous done. Treat this as
                // an empty update and drop any stale cached primary.
                state.reset_output_order();
                return;
            }
            state.output_order_done = true;
        }
        _ => {}
    }
}

impl Dispatch<xdg_activation_token_v1::XdgActivationTokenV1, ActivationTokenData> for Globals {
    fn event(
        self_: &mut Self,
        token: &xdg_activation_token_v1::XdgActivationTokenV1,
        event: xdg_activation_token_v1::Event,
        data: &ActivationTokenData,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // `Window.onActivationTokenEvent`. Zig compares the token with the
        // window's current one; a replaced token has already been destroyed
        // (and so receives no events) in the port.
        let Some(activation) = self_.get::<xdg_activation_v1::XdgActivationV1>() else {
            return;
        };

        if let xdg_activation_token_v1::Event::Done { token: done_token } = event {
            activation.activate(done_token, &data.surface);
            token.destroy();
        }
    }
}

impl Dispatch<vicinae_hotkey_v1::VicinaeHotkeyV1, HotkeyData> for Globals {
    fn event(
        self_: &mut Self,
        _: &vicinae_hotkey_v1::VicinaeHotkeyV1,
        event: vicinae_hotkey_v1::Event,
        data: &HotkeyData,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        self_.hotkey_events.push((data.id, event));
    }
}

wayland_client::delegate_noop!(Globals: ignore wl_compositor::WlCompositor);
wayland_client::delegate_noop!(Globals: ignore wl_region::WlRegion);
wayland_client::delegate_noop!(Globals: ignore org_kde_kwin_server_decoration::OrgKdeKwinServerDecoration);
wayland_client::delegate_noop!(Globals: ignore org_kde_kwin_slide_manager::OrgKdeKwinSlideManager);
wayland_client::delegate_noop!(Globals: ignore org_kde_kwin_slide::OrgKdeKwinSlide);
wayland_client::delegate_noop!(Globals: ignore xdg_activation_v1::XdgActivationV1);
wayland_client::delegate_noop!(Globals: ignore ext_background_effect_surface_v1::ExtBackgroundEffectSurfaceV1);
wayland_client::delegate_noop!(Globals: ignore vicinae_hotkey_manager_v1::VicinaeHotkeyManagerV1);

/// Receives the queued hotkey events after each dispatch.
pub type HotkeySink = Box<dyn Fn(u64, vicinae_hotkey_v1::Event)>;

/// The heap-allocated `Globals` (Zig allocates it so the listeners have a
/// stable address), together with its event queue and main-loop source.
pub struct Shared {
    pub conn: Connection,
    pub qh: QueueHandle<Globals>,
    queue: RefCell<EventQueue<Globals>>,
    pub globals: RefCell<Globals>,
    hotkey_sink: RefCell<Option<HotkeySink>>,
    source: Cell<*mut glib::ffi::GSource>,
}

impl Shared {
    fn init(display: &wl_display::WlDisplay) -> Result<Rc<Shared>, Error> {
        let backend = display.backend().upgrade().ok_or(Error::NoWaylandDisplay)?;
        let conn = Connection::from_backend(backend);
        let mut queue = conn.new_event_queue::<Globals>();
        let qh = queue.handle();
        let mut globals = Globals::new();

        let _registry = display.get_registry(&qh, ());
        if queue.roundtrip(&mut globals).is_err() {
            return Err(Error::RoundtripFailed);
        }

        // Do another roundtrip to process events emitted by globals we bound
        // during registry discovery (e.g. default decoration mode, output
        // order). Listeners are installed at bind time in registryListener.
        if globals.needs_roundtrip && queue.roundtrip(&mut globals).is_err() {
            return Err(Error::RoundtripFailed);
        }

        let shared = Rc::new(Shared {
            conn,
            qh,
            queue: RefCell::new(queue),
            globals: RefCell::new(globals),
            hotkey_sink: RefCell::new(None),
            source: Cell::new(std::ptr::null_mut()),
        });
        shared.attach_source();
        Ok(shared)
    }

    /// Sets the receiver of hotkey events.
    pub fn set_hotkey_sink(&self, sink: Option<HotkeySink>) {
        *self.hotkey_sink.borrow_mut() = sink;
    }

    /// Whether our queue has events waiting for dispatch.
    fn has_pending(&self) -> bool {
        // `prepare_read` fails when libwayland already queued events for us.
        self.queue.borrow().prepare_read().is_none()
    }

    /// Dispatches our queue, then delivers the queued hotkey events.
    pub fn dispatch(&self) {
        let events = {
            let (Ok(mut queue), Ok(mut globals)) =
                (self.queue.try_borrow_mut(), self.globals.try_borrow_mut())
            else {
                // Re-entered while dispatching; the outer call handles it.
                return;
            };
            if let Err(err) = queue.dispatch_pending(&mut globals) {
                log::warn!(target: LOG, "error dispatching wayland events err={err}");
            }
            let _ = queue.flush();
            std::mem::take(&mut globals.hotkey_events)
        };

        if events.is_empty() {
            return;
        }
        let sink = self.hotkey_sink.borrow();
        if let Some(sink) = sink.as_ref() {
            for (id, event) in events {
                sink(id, event);
            }
        }
    }

    /// Flushes requests to the compositor.
    pub fn flush(&self) {
        let _ = self.conn.flush();
    }

    fn attach_source(self: &Rc<Self>) {
        #[repr(C)]
        struct QueueSource {
            base: glib::ffi::GSource,
            shared: *const Shared,
        }

        unsafe fn shared_of(source: *mut glib::ffi::GSource) -> Option<Rc<Shared>> {
            let ptr = (*(source as *mut QueueSource)).shared;
            // SAFETY: `shared` is a `Weak::into_raw` pointer owned by the
            // source (released in `finalize`).
            let weak = std::mem::ManuallyDrop::new(Weak::from_raw(ptr));
            weak.upgrade()
        }

        unsafe extern "C" fn prepare(source: *mut glib::ffi::GSource, timeout: *mut c_int) -> glib::ffi::gboolean {
            *timeout = -1;
            match shared_of(source) {
                Some(shared) => shared.has_pending() as glib::ffi::gboolean,
                None => glib::ffi::GFALSE,
            }
        }

        unsafe extern "C" fn check(_: *mut glib::ffi::GSource) -> glib::ffi::gboolean {
            // GDK reads the socket in its own source; dispatch our queue
            // on every iteration so nothing it read for us is left behind
            // (dispatching an empty queue is cheap).
            glib::ffi::GTRUE
        }

        unsafe extern "C" fn dispatch(
            source: *mut glib::ffi::GSource,
            _: glib::ffi::GSourceFunc,
            _: glib::ffi::gpointer,
        ) -> glib::ffi::gboolean {
            if let Some(shared) = shared_of(source) {
                shared.dispatch();
            }
            glib::ffi::GTRUE
        }

        unsafe extern "C" fn finalize(source: *mut glib::ffi::GSource) {
            let ptr = (*(source as *mut QueueSource)).shared;
            drop(Weak::from_raw(ptr));
        }

        static mut FUNCS: glib::ffi::GSourceFuncs = glib::ffi::GSourceFuncs {
            prepare: Some(prepare),
            check: Some(check),
            dispatch: Some(dispatch),
            finalize: Some(finalize),
            closure_callback: None,
            closure_marshal: None,
        };

        // SAFETY: GLib allocates a QueueSource-sized source and never
        // writes to FUNCS; we initialize our field before attaching.
        unsafe {
            let source = glib::ffi::g_source_new(
                std::ptr::addr_of_mut!(FUNCS),
                std::mem::size_of::<QueueSource>() as u32,
            );
            (*(source as *mut QueueSource)).shared = Weak::into_raw(Rc::downgrade(self));
            glib::ffi::g_source_attach(source, std::ptr::null_mut());
            self.source.set(source);
        }
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        let source = self.source.replace(std::ptr::null_mut());
        if !source.is_null() {
            // SAFETY: we hold the reference from g_source_new.
            unsafe {
                glib::ffi::g_source_destroy(source);
                glib::ffi::g_source_unref(source);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_order_tracks_first_output_after_done() {
        let mut state = State::default();
        output_order_event(&mut state, kde_output_order_v1::Event::Output { output_name: "DP-1".into() });
        output_order_event(&mut state, kde_output_order_v1::Event::Output { output_name: "HDMI-1".into() });
        assert_eq!(state.primary_output_name.as_deref(), Some("DP-1"));
        output_order_event(&mut state, kde_output_order_v1::Event::Done);
        assert!(state.output_order_done);

        output_order_event(&mut state, kde_output_order_v1::Event::Output { output_name: "HDMI-1".into() });
        output_order_event(&mut state, kde_output_order_v1::Event::Done);
        assert_eq!(state.primary_output_name.as_deref(), Some("HDMI-1"));

        // A done without outputs drops the stale primary.
        output_order_event(&mut state, kde_output_order_v1::Event::Done);
        assert_eq!(state.primary_output_name, None);

        // Empty names are ignored.
        output_order_event(&mut state, kde_output_order_v1::Event::Output { output_name: String::new() });
        assert_eq!(state.primary_output_name, None);
        assert!(!state.output_order_done);
    }

    #[test]
    fn tag_interface_names() {
        assert_eq!(Tag::Compositor.interface_name(), "wl_compositor");
        assert_eq!(Tag::VicinaeHotkeyManager.interface_name(), "vicinae_hotkey_manager_v1");
        assert_eq!(Tag::KdeDecorationManager.interface_name(), "org_kde_kwin_server_decoration_manager");
        assert_eq!(Tag::XdgActivation.interface_name(), "xdg_activation_v1");
        for (i, tag) in Tag::ALL.iter().enumerate() {
            assert_eq!(tag.index(), i);
        }
    }
}
