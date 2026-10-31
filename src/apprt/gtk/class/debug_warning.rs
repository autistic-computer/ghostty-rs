//! Port of `src/apprt/gtk/class/debug_warning.zig`.
//!
//! STATUS: ported
#![cfg(feature = "gtk")]

use gtk4::subclass::prelude::*;
use libadwaita::subclass::prelude::*;

use crate::apprt::gtk::class::{self, Blueprint};

/// The template of this class (Zig: `gresource.blueprint(...)`).
pub const BLUEPRINT: Blueprint = Blueprint { major: 1, minor: 3, name: "debug-warning" };

mod imp {
    use super::*;

    #[derive(Default, gtk4::CompositeTemplate)]
    #[template(resource = "/com/mitchellh/ghostty/ui/1.3/debug-warning.ui")]
    pub struct DebugWarning {}

    #[glib::object_subclass]
    impl ObjectSubclass for DebugWarning {
        const NAME: &'static str = "GhosttyDebugWarning";
        type Type = super::DebugWarning;
        type ParentType = libadwaita::Bin;

        fn class_init(klass: &mut Self::Class) {
            class::ensure_resources();
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for DebugWarning {
        fn dispose(&self) {
            self.dispose_template();
        }
    }
    impl WidgetImpl for DebugWarning {}
    impl BinImpl for DebugWarning {}
}

glib::wrapper! {
    /// Debug warning banner.
    pub struct DebugWarning(ObjectSubclass<imp::DebugWarning>)
        @extends libadwaita::Bin, gtk4::Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget;
}

impl Default for DebugWarning {
    fn default() -> Self {
        glib::Object::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apprt::gtk::ext::test_gtk;
    use libadwaita::prelude::*;

    #[test]
    fn blueprint_path() {
        assert_eq!(class::blueprint(BLUEPRINT), "/com/mitchellh/ghostty/ui/1.3/debug-warning.ui");
    }

    #[test]
    fn instantiate() {
        test_gtk::run(|| {
            let w = DebugWarning::default();
            assert!(w.child().is_some());
        });
    }
}
