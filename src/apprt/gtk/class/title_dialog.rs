//! Port of `src/apprt/gtk/class/title_dialog.zig`.
//!
//! STATUS: ported
#![cfg(feature = "gtk")]

use std::cell::{Cell, RefCell};
use std::sync::LazyLock;

use glib::prelude::*;
use glib::subclass::Signal;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use libadwaita::prelude::*;
use libadwaita::subclass::prelude::*;

use crate::apprt::gtk::class::{self, accessor, i18n_, Blueprint};
use crate::apprt::gtk::ext;

/// The template of this class (Zig: `gresource.blueprint(...)`).
pub const BLUEPRINT: Blueprint = Blueprint { major: 1, minor: 5, name: "title-dialog" };

/// Property names (Zig: `properties`).
pub mod properties {
    pub const TARGET: &str = "target";
    pub const INITIAL_VALUE: &str = "initial-value";
}

/// Signal names (Zig: `signals`).
pub mod signals {
    /// Set the title to the given value.
    pub const SET: &str = "set";
}

mod imp {
    use super::*;

    #[derive(Default, gtk4::CompositeTemplate)]
    #[template(resource = "/com/mitchellh/ghostty/ui/1.5/title-dialog.ui")]
    pub struct TitleDialog {
        /// The initial value of the entry field.
        pub initial_value: RefCell<Option<String>>,

        // Template bindings
        pub target: Cell<Target>,
        #[template_child]
        pub entry: TemplateChild<gtk4::Entry>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TitleDialog {
        const NAME: &'static str = "GhosttyTitleDialog";
        type Type = super::TitleDialog;
        type ParentType = libadwaita::AlertDialog;

        fn class_init(klass: &mut Self::Class) {
            class::ensure_resources();
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for TitleDialog {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPS: LazyLock<Vec<glib::ParamSpec>> = LazyLock::new(|| {
                vec![
                    glib::ParamSpecString::builder(properties::INITIAL_VALUE).build(),
                    glib::ParamSpecEnum::builder_with_default(properties::TARGET, Target::Surface)
                        .build(),
                ]
            });
            PROPS.as_ref()
        }

        fn property(&self, _: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                properties::INITIAL_VALUE => accessor::get(&self.initial_value),
                properties::TARGET => accessor::get_shallow(&self.target),
                _ => unreachable!(),
            }
        }

        fn set_property(&self, _: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            match pspec.name() {
                properties::INITIAL_VALUE => accessor::set(&self.initial_value, value),
                properties::TARGET => accessor::set_shallow(&self.target, value),
                _ => unreachable!(),
            }
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: LazyLock<Vec<Signal>> = LazyLock::new(|| {
                vec![Signal::builder(signals::SET).param_types([String::static_type()]).build()]
            });
            SIGNALS.as_ref()
        }

        fn dispose(&self) {
            self.dispose_template();
        }
    }

    impl WidgetImpl for TitleDialog {}
    impl AdwDialogImpl for TitleDialog {}
    impl AdwAlertDialogImpl for TitleDialog {}
}

glib::wrapper! {
    pub struct TitleDialog(ObjectSubclass<imp::TitleDialog>)
        @extends libadwaita::AlertDialog, libadwaita::Dialog, gtk4::Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget;
}

impl TitleDialog {
    pub fn new(target: Target, initial_value: Option<&str>) -> TitleDialog {
        glib::Object::builder()
            .property(properties::TARGET, target)
            .property(properties::INITIAL_VALUE, initial_value)
            .build()
    }

    pub fn present(&self, parent: &gtk4::Widget) {
        // If we have a window we can attach to, we prefer that.
        let parent: gtk4::Widget =
            if let Some(window) = ext::get_ancestor::<libadwaita::ApplicationWindow>(parent) {
                window.upcast()
            } else if let Some(window) = ext::get_ancestor::<libadwaita::Window>(parent) {
                window.upcast()
            } else {
                parent.clone()
            };

        // Set our initial value
        let imp = self.imp();
        if let Some(v) = imp.initial_value.borrow().as_deref() {
            imp.entry.buffer().set_text(v);
        }

        // Set the title for the dialog
        self.set_heading(Some(imp.target.get().title()));

        // Show it. We could also just use virtual methods to bind to
        // response but this is pretty simple.
        let this = self.clone();
        self.clone().choose(Some(&parent), None::<&gio::Cancellable>, move |response| {
            this.alert_dialog_ready(&response);
        });
    }

    fn alert_dialog_ready(&self, response: &str) {
        // If we didn't hit "okay" then we do nothing.
        if response != "ok" {
            return;
        }

        // Emit our signal with the new title.
        let title = self.imp().entry.buffer().text();
        self.emit_by_name::<()>(signals::SET, &[&title.as_str()]);
    }

    /// `signals.set.connect`
    pub fn connect_set<F: Fn(&Self, &str) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_local(signals::SET, false, move |values| {
            let this: Self = values[0].get().unwrap();
            let title: String = values[1].get().unwrap();
            f(&this, &title);
            None
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, glib::Enum)]
#[repr(i32)]
#[enum_type(name = "GhosttyTitleDialogTarget")]
pub enum Target {
    #[default]
    #[enum_value(name = "surface", nick = "surface")]
    Surface,
    #[enum_value(name = "tab", nick = "tab")]
    Tab,
    #[enum_value(name = "window", nick = "window")]
    Window,
}

impl Target {
    pub fn title(self) -> &'static str {
        match self {
            Target::Surface => i18n_(c"Change Terminal Title"),
            Target::Tab => i18n_(c"Change Tab Title"),
            Target::Window => i18n_(c"Change Window Title"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apprt::gtk::ext::test_gtk;
    use std::rc::Rc;

    #[test]
    fn blueprint_path() {
        assert_eq!(class::blueprint(BLUEPRINT), "/com/mitchellh/ghostty/ui/1.5/title-dialog.ui");
    }

    #[test]
    fn instantiate_and_set() {
        test_gtk::run(|| {
            let dialog = TitleDialog::new(Target::Tab, Some("hello"));
            assert_eq!(dialog.property::<Target>(properties::TARGET), Target::Tab);
            assert_eq!(
                dialog.property::<Option<String>>(properties::INITIAL_VALUE).as_deref(),
                Some("hello")
            );

            let got = Rc::new(std::cell::RefCell::new(None));
            let g = got.clone();
            dialog.connect_set(move |_, t| *g.borrow_mut() = Some(t.to_owned()));

            dialog.imp().entry.buffer().set_text("new title");
            dialog.alert_dialog_ready("cancel");
            assert!(got.borrow().is_none());
            dialog.alert_dialog_ready("ok");
            assert_eq!(got.borrow().as_deref(), Some("new title"));
        });
    }
}
