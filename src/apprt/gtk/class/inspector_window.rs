//! Port of `src/apprt/gtk/class/inspector_window.zig`.
//!
//! STATUS: ported. Only built with the `inspector` Cargo feature, like
//! the [`InspectorWidget`] it embeds.
#![cfg(all(feature = "gtk", feature = "inspector"))]

use std::cell::RefCell;
use std::sync::LazyLock;

use glib::prelude::*;
use glib::subclass::prelude::*;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use libadwaita::subclass::prelude::*;

use crate::apprt::gtk::class::debug_warning::DebugWarning;
use crate::apprt::gtk::class::inspector_widget::InspectorWidget;
use crate::apprt::gtk::class::{self, Blueprint};
use crate::apprt::gtk::weak_ref::WeakRef;
use crate::build_config;

use crate::apprt::gtk::class::surface::Surface;

/// The template of this class (Zig: `gresource.blueprint(...)`).
pub const BLUEPRINT: Blueprint = Blueprint { major: 1, minor: 4, name: "inspector-window" };

/// Property names (Zig: `properties`).
pub mod properties {
    pub const SURFACE: &str = "surface";
    pub const DEBUG: &str = "debug";
}

mod imp {
    use super::*;

    #[derive(Default, gtk4::CompositeTemplate)]
    #[template(resource = "/com/mitchellh/ghostty/ui/1.4/inspector-window.ui")]
    pub struct InspectorWindow {
        /// The surface that we are attached to
        pub surface: RefCell<WeakRef<Surface>>,

        /// The embedded inspector widget.
        #[template_child]
        pub inspector_widget: TemplateChild<InspectorWidget>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for InspectorWindow {
        const NAME: &'static str = "GhosttyInspectorWindow";
        type Type = super::InspectorWindow;
        type ParentType = libadwaita::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            DebugWarning::ensure_type();
            InspectorWidget::ensure_type();
            class::ensure_resources();
            klass.bind_template();

            // Template callbacks
            class::bind_template_callback(klass, "notify_inspector_surface", |args| {
                // (inspector, pspec, self)
                let inspector: InspectorWidget = args.get(0);
                let this: super::InspectorWindow = args.template();
                this.prop_inspector_surface(&inspector);
                None
            });
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();

            // SAFETY: the instance is initialized (as in `init_template`).
            let this = unsafe { obj.as_ref() };

            // Add our dev CSS class if we're in debug mode.
            if build_config::IS_DEBUG {
                this.add_css_class("devel");
            }

            // Set our window icon. We can't set this in the blueprint file
            // because its dependent on the build config.
            this.set_icon_name(Some(build_config::BUNDLE_ID));
        }
    }

    impl ObjectImpl for InspectorWindow {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPS: LazyLock<Vec<glib::ParamSpec>> = LazyLock::new(|| {
                vec![
                    glib::ParamSpecObject::builder::<Surface>(properties::SURFACE).build(),
                    glib::ParamSpecBoolean::builder(properties::DEBUG)
                        .default_value(build_config::IS_DEBUG)
                        .read_only()
                        .build(),
                ]
            });
            PROPS.as_ref()
        }

        fn property(&self, _: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                // Important: get() refs, so the value takes that reference.
                properties::SURFACE => self.surface.borrow().get().to_value(),
                properties::DEBUG => build_config::IS_DEBUG.to_value(),
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
            // You MUST clear all weak refs in dispose, otherwise it causes
            // memory corruption on dispose on the TARGET (weak referenced)
            // object. The only way we caught this is via Valgrind. Its not a leak,
            // its an invalid memory read. In practice, I found this sometimes
            // caused hanging!
            self.obj().set_surface(None);

            self.dispose_template();
        }
    }

    impl WidgetImpl for InspectorWindow {}
    impl WindowImpl for InspectorWindow {}
    impl ApplicationWindowImpl for InspectorWindow {}
    impl AdwApplicationWindowImpl for InspectorWindow {}
}

glib::wrapper! {
    /// Window for displaying the Ghostty inspector.
    pub struct InspectorWindow(ObjectSubclass<imp::InspectorWindow>)
        @extends libadwaita::ApplicationWindow, gtk4::ApplicationWindow, gtk4::Window,
            gtk4::Widget,
        @implements gio::ActionGroup, gio::ActionMap, gtk4::Accessible, gtk4::Buildable,
            gtk4::ConstraintTarget, gtk4::Native, gtk4::Root, gtk4::ShortcutManager;
}

impl InspectorWindow {
    pub fn new(surface: &Surface) -> InspectorWindow {
        glib::Object::builder().property(properties::SURFACE, surface).build()
    }

    /// Present the window.
    pub fn present(&self) {
        GtkWindowExt::present(self);
    }

    /// Queue a render of the embedded widget.
    pub fn queue_render(&self) {
        self.imp().inspector_widget.queue_render();
    }

    fn set_surface(&self, newvalue: Option<&Surface>) {
        self.imp().surface.borrow_mut().set(newvalue);
    }

    //---------------------------------------------------------------
    // Signal Handlers

    fn prop_inspector_surface(&self, inspector: &InspectorWidget) {
        // If the inspector's surface went away, we destroy the window.
        // The inspector has a weak notify on the surface so it knows
        // if it goes nil.
        if inspector.get_surface().is_none() {
            self.destroy();
        }
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
            "/com/mitchellh/ghostty/ui/1.4/inspector-window.ui"
        );
    }

    #[test]
    fn instantiate_and_bind_surface() {
        test_gtk::run(|| {
            // A surface needs a live core surface (a running terminal) to
            // host the inspector; binding a surface without one makes the
            // inspector widget drop it, which closes the window (like Zig).
            // So the window is instantiated without a surface here.
            let window: InspectorWindow = glib::Object::new();
            assert!(window.property::<Option<Surface>>(properties::SURFACE).is_none());
            assert_eq!(window.property::<bool>(properties::DEBUG), build_config::IS_DEBUG);
            assert_eq!(window.icon_name().as_deref(), Some(build_config::BUNDLE_ID));
            assert!(window.imp().inspector_widget.get_surface().is_none());
            window.queue_render();

            window.destroy();
        });
    }
}
