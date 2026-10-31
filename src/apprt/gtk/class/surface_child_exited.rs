//! Port of `src/apprt/gtk/class/surface_child_exited.zig`.
//!
//! STATUS: ported
#![cfg(feature = "gtk")]

use std::cell::RefCell;
use std::sync::LazyLock;

use glib::prelude::*;
use glib::subclass::Signal;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use libadwaita::subclass::prelude::*;

use crate::apprt::gtk::class::{self, accessor, boxed, i18n_, Blueprint};
use crate::apprt::surface::ChildExited;

/// The template of this class (Zig: `gresource.blueprint(...)`).
pub const BLUEPRINT: Blueprint = Blueprint { major: 1, minor: 3, name: "surface-child-exited" };

/// Property names (Zig: `properties`).
pub mod properties {
    pub const DATA: &str = "data";
}

/// Signal names (Zig: `signals`).
pub mod signals {
    /// Emitted when the banner would like to be closed.
    pub const CLOSE_REQUEST: &str = "close-request";
}

mod imp {
    use super::*;

    #[derive(Default, gtk4::CompositeTemplate)]
    #[template(resource = "/com/mitchellh/ghostty/ui/1.3/surface-child-exited.ui")]
    pub struct SurfaceChildExited {
        /// The child exited data sent by the apprt.
        pub data: RefCell<Option<boxed::ChildExited>>,

        // Template bindings
        #[template_child]
        pub banner: TemplateChild<libadwaita::Banner>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SurfaceChildExited {
        const NAME: &'static str = "GhosttySurfaceChildExited";
        type Type = super::SurfaceChildExited;
        type ParentType = libadwaita::Bin;

        fn class_init(klass: &mut Self::Class) {
            class::ensure_resources();
            klass.bind_template();

            // Template bindings
            class::bind_template_callback(klass, "clicked", |args| {
                // (banner: *adw.Banner, self: *Self)
                let this: super::SurfaceChildExited = args.template();
                this.close_button_clicked();
                None
            });
            class::bind_template_callback(klass, "notify_data", |args| {
                // (self: *Self, pspec: *gobject.ParamSpec, ud)
                let this: super::SurfaceChildExited = args.template();
                this.prop_data();
                None
            });
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for SurfaceChildExited {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPS: LazyLock<Vec<glib::ParamSpec>> = LazyLock::new(|| {
                vec![glib::ParamSpecBoxed::builder::<boxed::ChildExited>(properties::DATA).build()]
            });
            PROPS.as_ref()
        }

        fn property(&self, _: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                properties::DATA => accessor::get(&self.data),
                _ => unreachable!(),
            }
        }

        fn set_property(&self, _: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            match pspec.name() {
                properties::DATA => accessor::set(&self.data, value),
                _ => unreachable!(),
            }
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: LazyLock<Vec<Signal>> =
                LazyLock::new(|| vec![Signal::builder(signals::CLOSE_REQUEST).build()]);
            SIGNALS.as_ref()
        }

        fn dispose(&self) {
            self.dispose_template();
        }

        // finalize: `data` is dropped with the private struct.
    }

    impl WidgetImpl for SurfaceChildExited {}
    impl BinImpl for SurfaceChildExited {}
}

glib::wrapper! {
    /// Child exited overlay based on adw.Banner.
    pub struct SurfaceChildExited(ObjectSubclass<imp::SurfaceChildExited>)
        @extends libadwaita::Bin, gtk4::Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget;
}

impl Default for SurfaceChildExited {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl SurfaceChildExited {
    pub fn set_data(&self, data: Option<&ChildExited>) {
        let imp = self.imp();
        let Some(data) = data else {
            imp.data.replace(None);
            return;
        };

        imp.data.replace(Some(boxed::ChildExited(*data)));
        self.notify(properties::DATA);
    }

    /// `signals.@"close-request".connect`
    pub fn connect_close_request<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_local(signals::CLOSE_REQUEST, false, move |values| {
            f(&values[0].get::<Self>().unwrap());
            None
        })
    }

    //---------------------------------------------------------------
    // Signal handlers

    fn prop_data(&self) {
        let imp = self.imp();
        let banner = &imp.banner;
        let data = imp.data.borrow();
        let Some(data) = data.as_ref() else {
            // Not localized on purpose.
            banner.set_title("This is a bug in Ghostty. Please report it.");
            return;
        };
        if data.0.exit_code == 0 {
            banner.set_title(i18n_(c"Command Succeeded"));
            self.add_css_class("normal");
            self.remove_css_class("abnormal");
        } else {
            banner.set_title(i18n_(c"Command Failed"));
            self.remove_css_class("normal");
            self.add_css_class("abnormal");
        }
    }

    fn close_button_clicked(&self) {
        self.emit_by_name::<()>(signals::CLOSE_REQUEST, &[]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apprt::gtk::ext::test_gtk;
    use std::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn blueprint_path() {
        assert_eq!(
            class::blueprint(BLUEPRINT),
            "/com/mitchellh/ghostty/ui/1.3/surface-child-exited.ui"
        );
    }

    #[test]
    fn instantiate_and_set_data() {
        test_gtk::run(|| {
            let w = SurfaceChildExited::default();
            w.set_data(Some(&ChildExited { exit_code: 0, runtime_ms: 10 }));
            assert_eq!(w.imp().banner.title().as_str(), "Command Succeeded");
            assert!(w.has_css_class("normal"));
            w.set_data(Some(&ChildExited { exit_code: 1, runtime_ms: 10 }));
            assert_eq!(w.imp().banner.title().as_str(), "Command Failed");
            assert!(w.has_css_class("abnormal"));
            assert!(!w.has_css_class("normal"));

            let value = w.property::<Option<boxed::ChildExited>>(properties::DATA).unwrap();
            assert_eq!(value.0.exit_code, 1);

            let closed = Rc::new(Cell::new(false));
            let c = closed.clone();
            w.connect_close_request(move |_| c.set(true));
            w.imp().banner.emit_by_name::<()>("button-clicked", &[]);
            assert!(closed.get());
        });
    }
}
