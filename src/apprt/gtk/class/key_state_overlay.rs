//! Port of `src/apprt/gtk/class/key_state_overlay.zig`.
//!
//! STATUS: ported
#![cfg(feature = "gtk")]

use std::cell::{Cell, RefCell};
use std::sync::LazyLock;

use glib::prelude::*;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use libadwaita::subclass::prelude::*;

use crate::apprt::gtk::class::{self, accessor, Blueprint};
use crate::apprt::gtk::ext::StringList;

/// The template of this class (Zig: `gresource.blueprint(...)`).
pub const BLUEPRINT: Blueprint = Blueprint { major: 1, minor: 0, name: "key-state-overlay" };

/// Property names (Zig: `properties`).
pub mod properties {
    pub const TABLES: &str = "tables";
    pub const HAS_TABLES: &str = "has-tables";
    pub const SEQUENCE: &str = "sequence";
    pub const HAS_SEQUENCE: &str = "has-sequence";
    pub const VALIGN_TARGET: &str = "valign-target";
}

mod imp {
    use super::*;

    #[derive(gtk4::CompositeTemplate)]
    #[template(resource = "/com/mitchellh/ghostty/ui/1.0/key-state-overlay.ui")]
    pub struct KeyStateOverlay {
        /// The key table stack.
        pub tables: RefCell<Option<StringList>>,

        /// The key sequence.
        pub sequence: RefCell<Option<StringList>>,

        /// Target vertical alignment for the overlay.
        pub valign_target: Cell<gtk4::Align>,
    }

    impl Default for KeyStateOverlay {
        fn default() -> Self {
            KeyStateOverlay {
                tables: RefCell::new(None),
                sequence: RefCell::new(None),
                valign_target: Cell::new(gtk4::Align::End),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for KeyStateOverlay {
        const NAME: &'static str = "GhosttyKeyStateOverlay";
        type Type = super::KeyStateOverlay;
        type ParentType = libadwaita::Bin;

        fn class_init(klass: &mut Self::Class) {
            class::ensure_resources();
            klass.bind_template();

            // Template Callbacks
            class::bind_template_callback(klass, "on_drag_end", |args| {
                // (gesture, offset_x: f64, offset_y: f64, self)
                let this: super::KeyStateOverlay = args.template();
                this.on_drag_end(args.get::<f64>(1), args.get::<f64>(2));
                None
            });
            class::bind_template_callback(klass, "show_chevron", |args| {
                // (self, has_tables: bool, has_sequence: bool)
                Some(super::closure_show_chevron(args.get(1), args.get(2)).to_value())
            });
            class::bind_template_callback(klass, "has_state", |args| {
                Some(super::closure_has_state(args.get(1), args.get(2)).to_value())
            });
            class::bind_template_callback(klass, "tables_text", |args| {
                let tables: Option<StringList> = args.get(1);
                Some(super::closure_tables_text(tables.as_ref()).to_value())
            });
            class::bind_template_callback(klass, "sequence_text", |args| {
                let sequence: Option<StringList> = args.get(1);
                Some(super::closure_sequence_text(sequence.as_ref()).to_value())
            });
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for KeyStateOverlay {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPS: LazyLock<Vec<glib::ParamSpec>> = LazyLock::new(|| {
                vec![
                    glib::ParamSpecBoxed::builder::<StringList>(properties::TABLES).build(),
                    glib::ParamSpecBoolean::builder(properties::HAS_TABLES)
                        .default_value(false)
                        .read_only()
                        .build(),
                    glib::ParamSpecBoxed::builder::<StringList>(properties::SEQUENCE).build(),
                    glib::ParamSpecBoolean::builder(properties::HAS_SEQUENCE)
                        .default_value(false)
                        .read_only()
                        .build(),
                    glib::ParamSpecEnum::builder_with_default(
                        properties::VALIGN_TARGET,
                        gtk4::Align::End,
                    )
                    .build(),
                ]
            });
            PROPS.as_ref()
        }

        fn property(&self, _: usize, pspec: &glib::ParamSpec) -> glib::Value {
            let obj = self.obj();
            match pspec.name() {
                properties::TABLES => accessor::get(&self.tables),
                properties::HAS_TABLES => obj.get_has_tables().to_value(),
                properties::SEQUENCE => accessor::get(&self.sequence),
                properties::HAS_SEQUENCE => obj.get_has_sequence().to_value(),
                properties::VALIGN_TARGET => accessor::get_shallow(&self.valign_target),
                _ => unreachable!(),
            }
        }

        fn set_property(&self, _: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            let obj = self.obj();
            match pspec.name() {
                properties::TABLES => obj.set_tables(value.get().unwrap()),
                properties::SEQUENCE => obj.set_sequence(value.get().unwrap()),
                properties::VALIGN_TARGET => accessor::set_shallow(&self.valign_target, value),
                _ => unreachable!(),
            }
        }

        fn dispose(&self) {
            self.dispose_template();
        }

        // finalize: the lists are dropped with the private struct.
    }

    impl WidgetImpl for KeyStateOverlay {}
    impl BinImpl for KeyStateOverlay {}
}

glib::wrapper! {
    /// An overlay that displays the current key table stack and pending key sequence.
    /// This helps users understand what key bindings are active and what keys they've
    /// pressed in a multi-key sequence.
    pub struct KeyStateOverlay(ObjectSubclass<imp::KeyStateOverlay>)
        @extends libadwaita::Bin, gtk4::Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget;
}

impl Default for KeyStateOverlay {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl KeyStateOverlay {
    fn get_tables(&self) -> Option<StringList> {
        self.imp().tables.borrow().clone()
    }

    fn get_sequence(&self) -> Option<StringList> {
        self.imp().sequence.borrow().clone()
    }

    fn set_tables(&self, value: Option<StringList>) {
        let _old = self.imp().tables.replace(value);
        self.notify(properties::TABLES);
        self.notify(properties::HAS_TABLES);
    }

    fn set_sequence(&self, value: Option<StringList>) {
        let _old = self.imp().sequence.replace(value);
        self.notify(properties::SEQUENCE);
        self.notify(properties::HAS_SEQUENCE);
    }

    fn get_has_tables(&self) -> bool {
        self.imp().tables.borrow().as_ref().is_some_and(|v| !v.strings.is_empty())
    }

    fn get_has_sequence(&self) -> bool {
        self.imp().sequence.borrow().as_ref().is_some_and(|v| !v.strings.is_empty())
    }

    //---------------------------------------------------------------
    // Template callbacks

    #[allow(deprecated)] // Zig uses getAllocatedHeight too.
    fn on_drag_end(&self, _offset_x: f64, offset_y: f64) {
        // Key state overlay only moves between top-center and bottom-center.
        // Horizontal alignment is always center.
        let priv_ = self.imp();
        let Some(parent) = self.parent() else { return };

        let parent_height = parent.allocated_height() as f64;
        let self_height = self.allocated_height() as f64;

        let self_y = if priv_.valign_target.get() == gtk4::Align::Start {
            0.0
        } else {
            parent_height - self_height
        };
        let new_y = self_y + offset_y + (self_height / 2.0);

        let new_valign =
            if new_y > parent_height / 2.0 { gtk4::Align::End } else { gtk4::Align::Start };

        if new_valign != priv_.valign_target.get() {
            priv_.valign_target.set(new_valign);
            self.notify(properties::VALIGN_TARGET);
            self.queue_resize();
        }
    }
}

fn closure_show_chevron(has_tables: bool, has_sequence: bool) -> bool {
    has_tables && has_sequence
}

fn closure_has_state(has_tables: bool, has_sequence: bool) -> bool {
    has_tables || has_sequence
}

fn join(list: Option<&StringList>, sep: &str) -> Option<String> {
    let list = list?;
    if list.strings.is_empty() {
        return None;
    }

    let mut buf = String::new();
    for (i, s) in list.strings.iter().enumerate() {
        if i > 0 {
            buf.push_str(sep);
        }
        buf.push_str(&s.to_string_lossy());
    }
    Some(buf)
}

fn closure_tables_text(tables: Option<&StringList>) -> Option<String> {
    join(tables, " > ")
}

fn closure_sequence_text(sequence: Option<&StringList>) -> Option<String> {
    join(sequence, " ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apprt::gtk::ext::test_gtk;

    #[test]
    fn blueprint_path() {
        assert_eq!(
            class::blueprint(BLUEPRINT),
            "/com/mitchellh/ghostty/ui/1.0/key-state-overlay.ui"
        );
    }

    #[test]
    fn closures() {
        assert!(closure_show_chevron(true, true));
        assert!(!closure_show_chevron(true, false));
        assert!(closure_has_state(false, true));
        assert!(!closure_has_state(false, false));
        let list = *StringList::create(&[c"a", c"b"]);
        assert_eq!(closure_tables_text(Some(&list)).as_deref(), Some("a > b"));
        assert_eq!(closure_sequence_text(Some(&list)).as_deref(), Some("a b"));
        assert_eq!(closure_tables_text(None), None);
        let empty = *StringList::create(&[]);
        assert_eq!(closure_sequence_text(Some(&empty)), None);
    }

    #[test]
    fn instantiate_and_bind() {
        test_gtk::run(|| {
            let o = KeyStateOverlay::default();
            assert!(!o.property::<bool>(properties::HAS_TABLES));
            // Hidden without state (bound via `has_state`).
            assert!(!o.is_visible());

            o.set_property(properties::TABLES, Some(*StringList::create(&[c"vim", c"g"])));
            assert!(o.property::<bool>(properties::HAS_TABLES));
            assert!(o.is_visible());
            assert_eq!(o.get_tables().unwrap().strings.len(), 2);

            o.set_property(properties::SEQUENCE, Some(*StringList::create(&[c"ctrl+a"])));
            assert!(o.property::<bool>(properties::HAS_SEQUENCE));
            assert_eq!(o.get_sequence().unwrap().strings.len(), 1);

            o.set_property(properties::TABLES, None::<StringList>);
            assert!(!o.property::<bool>(properties::HAS_TABLES));
            assert_eq!(o.property::<gtk4::Align>(properties::VALIGN_TARGET), gtk4::Align::End);
        });
    }
}
