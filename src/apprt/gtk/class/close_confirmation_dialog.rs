//! Port of `src/apprt/gtk/class/close_confirmation_dialog.zig`.
//!
//! STATUS: ported
#![cfg(feature = "gtk")]

use std::cell::Cell;
use std::sync::LazyLock;

use glib::prelude::*;
use glib::subclass::Signal;
use gtk4::subclass::prelude::*;
use libadwaita::prelude::*;
use libadwaita::subclass::prelude::*;

use crate::apprt::gtk::class::{self, accessor, i18n_, Blueprint};

/// The template of this class (Zig: `gresource.blueprint(...)`).
pub const BLUEPRINT: Blueprint =
    Blueprint { major: 1, minor: 5, name: "close-confirmation-dialog" };

/// Property names (Zig: `properties`).
pub mod properties {
    pub const TARGET: &str = "target";
}

/// Signal names (Zig: `signals`).
pub mod signals {
    pub const CLOSE_REQUEST: &str = "close-request";
    pub const CANCEL: &str = "cancel";
}

mod imp {
    use super::*;

    #[derive(Default, gtk4::CompositeTemplate)]
    #[template(resource = "/com/mitchellh/ghostty/ui/1.5/close-confirmation-dialog.ui")]
    pub struct CloseConfirmationDialog {
        pub target: Cell<Target>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CloseConfirmationDialog {
        const NAME: &'static str = "GhosttyCloseConfirmationDialog";
        type Type = super::CloseConfirmationDialog;
        type ParentType = libadwaita::AlertDialog;

        fn class_init(klass: &mut Self::Class) {
            class::ensure_resources();
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for CloseConfirmationDialog {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPS: LazyLock<Vec<glib::ParamSpec>> = LazyLock::new(|| {
                vec![glib::ParamSpecEnum::builder_with_default(properties::TARGET, Target::App)
                    .build()]
            });
            PROPS.as_ref()
        }

        fn property(&self, _: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                properties::TARGET => accessor::get_shallow(&self.target),
                _ => unreachable!(),
            }
        }

        fn set_property(&self, _: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            match pspec.name() {
                properties::TARGET => accessor::set_shallow(&self.target, value),
                _ => unreachable!(),
            }
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: LazyLock<Vec<Signal>> = LazyLock::new(|| {
                vec![
                    Signal::builder(signals::CLOSE_REQUEST).build(),
                    Signal::builder(signals::CANCEL).build(),
                ]
            });
            SIGNALS.as_ref()
        }

        fn dispose(&self) {
            self.dispose_template();
        }
    }

    impl WidgetImpl for CloseConfirmationDialog {}
    impl AdwDialogImpl for CloseConfirmationDialog {}

    impl AdwAlertDialogImpl for CloseConfirmationDialog {
        fn response(&self, response_id: &str) {
            if response_id == "close" {
                self.obj().emit_by_name::<()>(signals::CLOSE_REQUEST, &[]);
            } else {
                self.obj().emit_by_name::<()>(signals::CANCEL, &[]);
            }
        }
    }
}

glib::wrapper! {
    pub struct CloseConfirmationDialog(ObjectSubclass<imp::CloseConfirmationDialog>)
        @extends libadwaita::AlertDialog, libadwaita::Dialog, gtk4::Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget;
}

impl CloseConfirmationDialog {
    pub fn new(target: Target) -> CloseConfirmationDialog {
        glib::Object::builder().property(properties::TARGET, target).build()
    }

    pub fn present(&self, parent: Option<&gtk4::Widget>) {
        // Setup our title/body text.
        let target = self.imp().target.get();
        self.set_heading(Some(target.title()));
        self.set_body(target.body());

        // Show it
        AdwDialogExt::present(self, parent);
    }

    pub fn close(&self) {
        self.force_close();
    }

    /// `signals.@"close-request".connect`
    pub fn connect_close_request<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_local(signals::CLOSE_REQUEST, false, move |values| {
            f(&values[0].get::<Self>().unwrap());
            None
        })
    }

    /// `signals.cancel.connect`
    pub fn connect_cancel<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_local(signals::CANCEL, false, move |values| {
            f(&values[0].get::<Self>().unwrap());
            None
        })
    }
}

/// The target of a close dialog.
///
/// This is here so that we can consolidate all logic related to
/// prompting the user and closing windows/tabs/surfaces/etc.
/// together into one struct that is the sole source of truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, glib::Enum)]
#[repr(i32)]
#[enum_type(name = "GhosttyCloseConfirmationDialogTarget")]
pub enum Target {
    #[default]
    #[enum_value(name = "app", nick = "app")]
    App,
    #[enum_value(name = "tab", nick = "tab")]
    Tab,
    #[enum_value(name = "window", nick = "window")]
    Window,
    #[enum_value(name = "surface", nick = "surface")]
    Surface,
}

impl Target {
    pub fn title(self) -> &'static str {
        match self {
            Target::App => i18n_(c"Quit Ghostty?"),
            Target::Tab => i18n_(c"Close Tab?"),
            Target::Window => i18n_(c"Close Window?"),
            Target::Surface => i18n_(c"Close Split?"),
        }
    }

    pub fn body(self) -> &'static str {
        match self {
            Target::App => i18n_(c"All terminal sessions will be terminated."),
            Target::Tab => i18n_(c"All terminal sessions in this tab will be terminated."),
            Target::Window => i18n_(c"All terminal sessions in this window will be terminated."),
            Target::Surface => {
                i18n_(c"The currently running process in this split will be terminated.")
            }
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
        assert_eq!(
            class::blueprint(BLUEPRINT),
            "/com/mitchellh/ghostty/ui/1.5/close-confirmation-dialog.ui"
        );
    }

    #[test]
    fn target_text() {
        assert_eq!(Target::App.title(), "Quit Ghostty?");
        assert_eq!(Target::Surface.body(), "The currently running process in this split will be terminated.");
    }

    #[test]
    fn instantiate_and_respond() {
        test_gtk::run(|| {
            let dialog = CloseConfirmationDialog::new(Target::Tab);
            assert_eq!(dialog.property::<Target>(properties::TARGET), Target::Tab);

            let log = Rc::new(std::cell::RefCell::new(Vec::new()));
            let l = log.clone();
            dialog.connect_close_request(move |_| l.borrow_mut().push("close"));
            let l = log.clone();
            dialog.connect_cancel(move |_| l.borrow_mut().push("cancel"));
            dialog.emit_by_name::<()>("response", &[&"close"]);
            dialog.emit_by_name::<()>("response", &[&"cancel"]);
            assert_eq!(*log.borrow(), vec!["close", "cancel"]);
        });
    }
}
