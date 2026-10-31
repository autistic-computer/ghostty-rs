//! Port of `src/apprt/gtk/class/inspector_widget.zig`.
//!
//! STATUS: ported (only built with the `inspector` Cargo feature)
//!
//! Zig keeps an unreferenced surface pointer plus a weak notify and reads
//! the core surface from the dying surface in the notify. The port keeps a
//! `GWeakRef`, the pointer (for identity) and the core surface it
//! activated the inspector on, since a Rust wrapper can't be obtained for an
//! object that is being finalized.
#![cfg(all(feature = "gtk", feature = "inspector"))]

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::LazyLock;

use glib::prelude::*;
use glib::subclass::prelude::*;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use libadwaita::subclass::prelude::*;

use crate::apprt::gtk::class::imgui_widget::{ImguiWidget, ImguiWidgetImpl};
use crate::apprt::gtk::class::{self, Blueprint};

use crate::apprt::gtk::class::surface::Surface;
use crate::surface::Surface as CoreSurface;

const LOG: &str = "gtk_ghostty_inspector_widget";

/// The template of this class (Zig: `gresource.blueprint(...)`).
pub const BLUEPRINT: Blueprint = Blueprint { major: 1, minor: 0, name: "inspector-widget" };

/// Property names (Zig: `properties`).
pub mod properties {
    pub const SURFACE: &str = "surface";
}

use crate::inspector::Inspector;

mod imp {
    use super::*;

    #[derive(Default, gtk4::CompositeTemplate)]
    #[template(resource = "/com/mitchellh/ghostty/ui/1.0/inspector-widget.ui")]
    pub struct InspectorWidget {
        /// The surface that we are attached to. This is NOT referenced.
        /// We attach a weak notify to the object.
        pub surface: glib::WeakRef<Surface>,

        /// The identity of `surface` (Zig compares the pointers).
        pub surface_ptr: Cell<usize>,

        /// The core surface the inspector was activated on.
        pub core_surface: Cell<Option<*mut CoreSurface>>,

        /// The weak notify on `surface`.
        pub weak_notify: RefCell<Option<glib::object::WeakRefNotify<Surface>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for InspectorWidget {
        const NAME: &'static str = "GhosttyInspectorWidget";
        type Type = super::InspectorWidget;
        type ParentType = ImguiWidget;

        fn class_init(klass: &mut Self::Class) {
            ImguiWidget::ensure_type();
            Surface::ensure_type();
            class::ensure_resources();
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for InspectorWidget {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPS: LazyLock<Vec<glib::ParamSpec>> = LazyLock::new(|| {
                vec![glib::ParamSpecObject::builder::<Surface>(properties::SURFACE).build()]
            });
            PROPS.as_ref()
        }

        fn property(&self, _: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                properties::SURFACE => self.obj().get_surface().to_value(),
                _ => unreachable!(),
            }
        }

        fn set_property(&self, _: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            match pspec.name() {
                properties::SURFACE => {
                    let surface: Option<Surface> = value.get().unwrap();
                    self.obj().set_surface(surface.as_ref());
                }
                _ => unreachable!(),
            }
        }

        fn dispose(&self) {
            // Clear our surface so it deactivates the inspector.
            self.obj().set_surface(None);
            self.dispose_template();
        }
    }

    impl WidgetImpl for InspectorWidget {}
    impl BinImpl for InspectorWidget {}

    impl ImguiWidgetImpl for InspectorWidget {
        /// Called to do initial setup of the UI.
        fn setup(&self) {
            Inspector::setup();
        }

        /// Called for every frame to draw the UI.
        fn render(&self) {
            let Some(core_surface) = self.obj().get_surface().and_then(|s| s.core()) else {
                return;
            };
            // SAFETY: the core surface is live while its GTK surface is.
            let Some(inspector) = (unsafe { (*core_surface).inspector.as_deref_mut() }) else {
                return;
            };
            let inspector: *mut Inspector = inspector;
            // SAFETY: the inspector is owned by the live core surface and
            // the ImGui widget made its context current for this callback.
            unsafe { (*inspector).render(core_surface) };
        }
    }
}

glib::wrapper! {
    /// Widget for displaying the Ghostty inspector.
    pub struct InspectorWidget(ObjectSubclass<imp::InspectorWidget>)
        @extends ImguiWidget, libadwaita::Bin, gtk4::Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget;
}

impl Default for InspectorWidget {
    fn default() -> Self {
        glib::Object::new()
    }
}

fn object_ptr(surface: Option<&Surface>) -> usize {
    surface.map_or(0, |s| s.as_ptr() as usize)
}

impl InspectorWidget {
    //---------------------------------------------------------------
    // Public methods

    /// Queue a render of the Dear ImGui widget.
    pub fn queue_render(&self) {
        self.upcast_ref::<ImguiWidget>().queue_render();
    }

    //---------------------------------------------------------------
    // Properties

    pub fn get_surface(&self) -> Option<Surface> {
        self.imp().surface.upgrade()
    }

    pub fn set_surface(&self, surface: Option<&Surface>) {
        let priv_ = self.imp();

        // Do nothing if we're not changing the value.
        if object_ptr(surface) == priv_.surface_ptr.get() {
            return;
        }

        // Setup our notification to happen at the end because we're
        // changing values no matter what.
        let _guard = self.freeze_notify();
        self.notify(properties::SURFACE);

        // Deactivate the inspector on the old surface if it exists
        // and set our value to null.
        if priv_.surface_ptr.get() != 0 {
            priv_.surface_ptr.set(0);
            priv_.surface.set(None);

            // Remove our weak ref
            if let Some(notify) = priv_.weak_notify.take() {
                notify.disconnect();
            }

            // Deactivate the inspector
            if let Some(core_surface) = priv_.core_surface.take() {
                // SAFETY: the core surface outlives the weak notify (it is
                // freed in the surface's finalize).
                unsafe { (*core_surface).deactivate_inspector() };
            }
        }

        // Activate the inspector on the new surface.
        let Some(surface) = surface else { return };
        let Some(core_surface) = surface.core() else { return };
        // SAFETY: the core surface is live while its GTK surface is.
        if let Err(err) = unsafe { (*core_surface).activate_inspector() } {
            log::warn!(target: LOG, "failed to activate inspector err={err:?}");
            return;
        }

        // We use a weak reference on surface to determine if the surface
        // was closed while our inspector was active.
        let weak = self.downgrade();
        let ptr = object_ptr(Some(surface));
        let notify = surface.add_weak_ref_notify_local(move || {
            if let Some(this) = weak.upgrade() {
                this.surface_weak_notify(ptr);
            }
        });
        priv_.weak_notify.replace(Some(notify));

        // Store our surface. We don't need to ref this because we setup
        // the weak notify above.
        priv_.surface.set(Some(surface));
        priv_.surface_ptr.set(ptr);
        priv_.core_surface.replace(Some(core_surface));

        self.queue_render();
    }

    //---------------------------------------------------------------
    // Signal Handlers

    fn surface_weak_notify(&self, surface: usize) {
        let priv_ = self.imp();

        // The weak notify docs call out that we can specifically use the
        // pointer values for comparison, but the objects themselves are unsafe.
        if priv_.surface_ptr.get() != surface {
            return;
        }

        // The notify has fired, so there is nothing to disconnect.
        let _ = priv_.weak_notify.take();

        let Some(core_surface) = priv_.core_surface.take() else { return };
        // SAFETY: Zig's weak notify runs before the surface is finalized,
        // which is where the core surface is freed.
        unsafe { (*core_surface).deactivate_inspector() };
        priv_.surface_ptr.set(0);
        priv_.surface.set(None);
        self.notify(properties::SURFACE);

        // Note: in the future we should probably show some content on our
        // window to note that the surface went away in case our embedding
        // widget doesn't close itself. As I type this, our window closes
        // immediately when the surface goes away so you don't see this, but
        // for completeness sake we should clean this up.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apprt::gtk::ext::test_gtk;

    #[test]
    fn blueprint_path() {
        assert_eq!(
            class::blueprint(BLUEPRINT),
            "/com/mitchellh/ghostty/ui/1.0/inspector-widget.ui"
        );
    }

    #[test]
    fn surface_lifecycle() {
        test_gtk::run(|| {
            let w = InspectorWidget::default();
            // The subclass overrides the virtual methods. They need a
            // current ImGui context (the widget creates one on realize).
            // SAFETY: plain ImGui context lifecycle.
            let ctx = unsafe { crate::pkg::dcimgui::c::ImGui_CreateContext(std::ptr::null_mut()) };
            w.upcast_ref::<ImguiWidget>().setup();
            // SAFETY: the context is current.
            assert_ne!(unsafe { (*crate::pkg::dcimgui::c::ImGui_GetIO()).ConfigFlags }, 0);
            w.upcast_ref::<ImguiWidget>().render();
            // SAFETY: created above.
            unsafe { crate::pkg::dcimgui::c::ImGui_DestroyContext(ctx) };

            // A surface without a core surface (not yet realized) can't
            // host the inspector, so it isn't stored. (A live core surface
            // needs a running terminal; see the GTK app smoke test.)
            let surface = Surface::new(&crate::apprt::gtk::class::overrides::Overrides::NONE);
            let notified = std::rc::Rc::new(Cell::new(false));
            let n = notified.clone();
            w.connect_notify_local(Some(properties::SURFACE), move |_, _| n.set(true));
            w.set_property(properties::SURFACE, &surface);
            assert!(notified.get());
            assert!(w.get_surface().is_none());
            assert!(w.imp().core_surface.get().is_none());

            w.set_surface(None);
            assert!(w.get_surface().is_none());
        });
    }
}
