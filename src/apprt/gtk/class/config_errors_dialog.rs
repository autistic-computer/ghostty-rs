//! Port of `src/apprt/gtk/class/config_errors_dialog.zig`.
//!
//! STATUS: ported
#![cfg(feature = "gtk")]

use std::cell::RefCell;
use std::sync::LazyLock;

use glib::prelude::*;
use glib::subclass::Signal;
use gtk4::subclass::prelude::*;
use libadwaita::prelude::*;
use libadwaita::subclass::prelude::*;

use crate::apprt::gtk::class::{self, accessor, Blueprint, Config};

/// The template of this class (Zig: `gresource.blueprint(...)`).
pub const BLUEPRINT: Blueprint = Blueprint { major: 1, minor: 5, name: "config-errors-dialog" };

/// Property names (Zig: `properties`).
pub mod properties {
    pub const CONFIG: &str = "config";
}

/// Signal names (Zig: `signals`).
pub mod signals {
    pub const RELOAD_CONFIG: &str = "reload-config";
}

mod imp {
    use super::*;

    #[derive(Default, gtk4::CompositeTemplate)]
    #[template(resource = "/com/mitchellh/ghostty/ui/1.5/config-errors-dialog.ui")]
    pub struct ConfigErrorsDialog {
        pub config: RefCell<Option<Config>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ConfigErrorsDialog {
        const NAME: &'static str = "GhosttyConfigErrorsDialog";
        type Type = super::ConfigErrorsDialog;
        type ParentType = libadwaita::AlertDialog;

        fn class_init(klass: &mut Self::Class) {
            class::ensure_resources();
            Config::ensure_type();
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for ConfigErrorsDialog {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPS: LazyLock<Vec<glib::ParamSpec>> = LazyLock::new(|| {
                vec![glib::ParamSpecObject::builder::<Config>(properties::CONFIG).build()]
            });
            PROPS.as_ref()
        }

        fn property(&self, _: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                properties::CONFIG => accessor::get(&self.config),
                _ => unreachable!(),
            }
        }

        fn set_property(&self, _: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            match pspec.name() {
                // We don't need to increase the reference count because
                // the property setter handles it.
                properties::CONFIG => accessor::set(&self.config, value),
                _ => unreachable!(),
            }
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: LazyLock<Vec<Signal>> =
                LazyLock::new(|| vec![Signal::builder(signals::RELOAD_CONFIG).build()]);
            SIGNALS.as_ref()
        }

        fn dispose(&self) {
            self.config.replace(None);
            self.dispose_template();
        }
    }

    impl WidgetImpl for ConfigErrorsDialog {}
    impl AdwDialogImpl for ConfigErrorsDialog {}

    impl AdwAlertDialogImpl for ConfigErrorsDialog {
        fn response(&self, response_id: &str) {
            if response_id != "reload" {
                return;
            }
            self.obj().emit_by_name::<()>(signals::RELOAD_CONFIG, &[]);
        }
    }
}

glib::wrapper! {
    pub struct ConfigErrorsDialog(ObjectSubclass<imp::ConfigErrorsDialog>)
        @extends libadwaita::AlertDialog, libadwaita::Dialog, gtk4::Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget;
}

impl ConfigErrorsDialog {
    pub fn new(config: &Config) -> ConfigErrorsDialog {
        glib::Object::builder().property(properties::CONFIG, config).build()
    }

    pub fn present(&self, parent: Option<&gtk4::Widget>) {
        AdwDialogExt::present(self, parent);
    }

    pub fn close(&self) {
        self.force_close();
    }

    /// `getConfig`
    pub fn get_config(&self) -> Option<Config> {
        self.imp().config.borrow().clone()
    }

    /// `setConfig`
    pub fn set_config(&self, config: Option<&Config>) {
        self.set_property(properties::CONFIG, config);
    }

    /// `signals.@"reload-config".connect`
    pub fn connect_reload_config<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_local(signals::RELOAD_CONFIG, false, move |values| {
            let this: Self = values[0].get().unwrap();
            f(&this);
            None
        })
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
            "/com/mitchellh/ghostty/ui/1.5/config-errors-dialog.ui"
        );
    }

    #[test]
    fn instantiate_and_respond() {
        test_gtk::run(|| {
            let config = Config::new(&crate::config::config::Config::default());
            let dialog = ConfigErrorsDialog::new(&config);
            assert_eq!(dialog.get_config().as_ref(), Some(&config));

            let reloaded = Rc::new(Cell::new(0));
            let r = reloaded.clone();
            dialog.connect_reload_config(move |_| r.set(r.get() + 1));
            dialog.emit_by_name::<()>("response", &[&"ignore"]);
            assert_eq!(reloaded.get(), 0);
            dialog.emit_by_name::<()>("response", &[&"reload"]);
            assert_eq!(reloaded.get(), 1);

            dialog.set_config(None);
            assert!(dialog.get_config().is_none());
        });
    }
}
