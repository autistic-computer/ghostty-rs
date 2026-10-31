//! Port of `src/apprt/gtk/class/surface_scrolled_window.zig`.
//!
//! STATUS: ported.
//!
//! Zig stores the surface pointer without a reference (the scrolled
//! window, our child, keeps it alive); the port holds a strong reference,
//! which creates no cycle.
#![cfg(feature = "gtk")]

use std::cell::RefCell;
use std::sync::LazyLock;

use glib::prelude::*;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use libadwaita::subclass::prelude::*;

use crate::apprt::gtk::class::{self, accessor, Blueprint, Config};
use crate::apprt::gtk::gtk_version;
use crate::config::config::Scrollbar;

use crate::apprt::gtk::class::surface::Surface;

/// The template of this class (Zig: `gresource.blueprint(...)`).
pub const BLUEPRINT: Blueprint =
    Blueprint { major: 1, minor: 0, name: "surface-scrolled-window" };

/// Property names (Zig: `properties`).
pub mod properties {
    pub const CONFIG: &str = "config";
    pub const SURFACE: &str = "surface";
}

mod imp {
    use super::*;

    #[derive(Default, gtk4::CompositeTemplate)]
    #[template(resource = "/com/mitchellh/ghostty/ui/1.0/surface-scrolled-window.ui")]
    pub struct SurfaceScrolledWindow {
        pub config: RefCell<Option<Config>>,
        pub config_binding: RefCell<Option<glib::Binding>>,
        pub surface: RefCell<Option<Surface>>,
        #[template_child]
        pub scrolled_window: TemplateChild<gtk4::ScrolledWindow>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SurfaceScrolledWindow {
        // The triple "t" is the GType name in Zig (and the template).
        const NAME: &'static str = "GhostttySurfaceScrolledWindow";
        type Type = super::SurfaceScrolledWindow;
        type ParentType = libadwaita::Bin;

        fn class_init(klass: &mut Self::Class) {
            class::ensure_resources();
            Config::ensure_type();
            Surface::ensure_type();
            klass.bind_template();

            // Bindings
            class::bind_template_callback(klass, "scrollbar_policy", |args| {
                // (self, config: ?*Config) -> gtk.PolicyType
                let config: Option<Config> = args.get(1);
                Some(super::closure_scrollbar_policy(config.as_ref()).to_value())
            });
            class::bind_template_callback(klass, "notify_surface", |args| {
                let this: super::SurfaceScrolledWindow = args.template();
                this.prop_surface();
                None
            });
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
            if gtk_version::runtime_until(4, 20, 1) {
                // SAFETY: the instance is initialized (as in `init_template`).
                unsafe { obj.as_ref() }.disable_kinetic_scroll();
            }
        }
    }

    impl ObjectImpl for SurfaceScrolledWindow {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPS: LazyLock<Vec<glib::ParamSpec>> = LazyLock::new(|| {
                vec![
                    glib::ParamSpecObject::builder::<Config>(properties::CONFIG).build(),
                    glib::ParamSpecObject::builder::<Surface>(properties::SURFACE).build(),
                ]
            });
            PROPS.as_ref()
        }

        fn property(&self, _: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                properties::CONFIG => accessor::get(&self.config),
                properties::SURFACE => accessor::get(&self.surface),
                _ => unreachable!(),
            }
        }

        fn set_property(&self, _: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            match pspec.name() {
                properties::CONFIG => accessor::set(&self.config, value),
                properties::SURFACE => {
                    let surface: Option<Surface> = value.get().unwrap();
                    self.obj().set_surface(surface.as_ref());
                }
                _ => unreachable!(),
            }
        }

        fn dispose(&self) {
            if let Some(binding) = self.config_binding.take() {
                binding.unbind();
            }
            self.config.replace(None);
            self.dispose_template();
        }
    }

    impl WidgetImpl for SurfaceScrolledWindow {}
    impl BinImpl for SurfaceScrolledWindow {}
}

glib::wrapper! {
    /// A wrapper widget that embeds a Surface inside a GtkScrolledWindow.
    /// This provides scrollbar functionality for the terminal surface.
    /// The surface property can be set during initialization or changed
    /// dynamically via the surface property.
    pub struct SurfaceScrolledWindow(ObjectSubclass<imp::SurfaceScrolledWindow>)
        @extends libadwaita::Bin, gtk4::Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget;
}

impl Default for SurfaceScrolledWindow {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl SurfaceScrolledWindow {
    fn disable_kinetic_scroll(&self) {
        // Until gtk 4.20.1 trackpads have kinetic scrolling behavior regardless
        // of `Gtk.ScrolledWindow.kinetic_scrolling`. As a workaround, disable
        // EventControllerScroll.kinetic
        let controllers = self.imp().scrolled_window.observe_controllers();
        let mut i = 0;
        while let Some(obj) = controllers.item(i) {
            i += 1;
            let Ok(controller) = obj.downcast::<gtk4::EventControllerScroll>() else {
                continue;
            };
            let mut flags = controller.flags();
            flags.remove(gtk4::EventControllerScrollFlags::KINETIC);
            controller.set_flags(flags);
        }
    }

    pub fn get_surface(&self) -> Option<Surface> {
        self.imp().surface.borrow().clone()
    }

    pub fn set_surface(&self, surface: Option<&Surface>) {
        let priv_ = self.imp();

        if surface == priv_.surface.borrow().as_ref() {
            return;
        }

        let _guard = self.freeze_notify();
        self.notify(properties::SURFACE);

        priv_.surface.replace(surface.cloned());
    }

    /// Update our size request to match the smallest size our surface
    /// can be resized to within a split.
    pub fn sync_split_min_size(&self) {
        let Some(surface) = self.get_surface() else { return };
        let size = surface.get_split_min_size();
        self.set_size_request(size.width as i32, size.height as i32);
    }

    fn prop_surface(&self) {
        let priv_ = self.imp();
        let surface = self.get_surface();
        priv_.scrolled_window.set_child(surface.as_ref());
        self.sync_split_min_size();

        // Unbind old config binding if it exists
        if let Some(binding) = priv_.config_binding.take() {
            binding.unbind();
        }

        // Bind config from surface to our config property
        if let Some(surface) = surface {
            // The `glib::Binding` we keep is a reference, so the binding
            // can't be freed (and our pointer become stale) if the surface
            // gets finalized.
            let binding = surface
                .bind_property(properties::CONFIG, self, properties::CONFIG)
                .sync_create()
                .build();
            priv_.config_binding.replace(Some(binding));
        }
    }
}

fn closure_scrollbar_policy(config: Option<&Config>) -> gtk4::PolicyType {
    let Some(config) = config else { return gtk4::PolicyType::Automatic };
    match config.get().scrollbar {
        Scrollbar::Never => gtk4::PolicyType::Never,
        Scrollbar::System => gtk4::PolicyType::Automatic,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apprt::gtk::class::surface::Size;
    use crate::apprt::gtk::ext::test_gtk;

    #[test]
    fn blueprint_path() {
        assert_eq!(
            class::blueprint(BLUEPRINT),
            "/com/mitchellh/ghostty/ui/1.0/surface-scrolled-window.ui"
        );
    }

    #[test]
    fn instantiate_and_set_surface() {
        test_gtk::run(|| {
            let w = SurfaceScrolledWindow::default();
            assert_eq!(w.imp().scrolled_window.vscrollbar_policy(), gtk4::PolicyType::Automatic);

            let mut core = crate::config::config::Config::default();
            core.scrollbar = Scrollbar::Never;
            let config = Config::new(&core);

            let surface = Surface::new(&crate::apprt::gtk::class::overrides::Overrides::NONE);
            surface.set_property("config", &config);
            surface.set_split_min_size(Size { width: 30, height: 40 });

            w.set_property(properties::SURFACE, &surface);
            assert_eq!(w.get_surface().as_ref(), Some(&surface));
            // The stand-in surface is not scrollable, so GTK wraps it in a
            // viewport.
            assert!(surface.is_ancestor(&*w.imp().scrolled_window));
            assert_eq!(w.size_request(), (30, 40));

            // The config is bound from the surface and drives the policy.
            assert_eq!(w.property::<Option<Config>>(properties::CONFIG).as_ref(), Some(&config));
            assert_eq!(w.imp().scrolled_window.vscrollbar_policy(), gtk4::PolicyType::Never);

            w.set_surface(None);
            assert!(w.imp().scrolled_window.child().is_none());
        });
    }
}
