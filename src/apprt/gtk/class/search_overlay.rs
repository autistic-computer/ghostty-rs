//! Port of `src/apprt/gtk/class/search_overlay.zig`.
//!
//! STATUS: ported
#![cfg(feature = "gtk")]

use std::cell::Cell;
use std::sync::LazyLock;

use glib::prelude::*;
use glib::subclass::Signal;
use gtk4::gdk;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use libadwaita::subclass::prelude::*;

use crate::apprt::gtk::class::{self, accessor, Blueprint};

/// The template of this class (Zig: `gresource.blueprint(...)`).
pub const BLUEPRINT: Blueprint = Blueprint { major: 1, minor: 0, name: "search-overlay" };

/// Property names (Zig: `properties`).
pub mod properties {
    pub const ACTIVE: &str = "active";
    pub const SEARCH_TOTAL: &str = "search-total";
    pub const HAS_SEARCH_TOTAL: &str = "has-search-total";
    pub const SEARCH_SELECTED: &str = "search-selected";
    pub const HAS_SEARCH_SELECTED: &str = "has-search-selected";
    pub const HALIGN_TARGET: &str = "halign-target";
    pub const VALIGN_TARGET: &str = "valign-target";
}

/// Signal names (Zig: `signals`).
pub mod signals {
    /// Emitted when the search is stopped (e.g., Escape pressed).
    pub const STOP_SEARCH: &str = "stop-search";
    /// Emitted when the search text changes (debounced).
    pub const SEARCH_CHANGED: &str = "search-changed";
    /// Emitted when navigating to the next match.
    pub const NEXT_MATCH: &str = "next-match";
    /// Emitted when navigating to the previous match.
    pub const PREVIOUS_MATCH: &str = "previous-match";
}

mod imp {
    use super::*;

    #[derive(gtk4::CompositeTemplate)]
    #[template(resource = "/com/mitchellh/ghostty/ui/1.0/search-overlay.ui")]
    pub struct SearchOverlay {
        /// The search entry widget.
        #[template_child]
        pub search_entry: TemplateChild<gtk4::SearchEntry>,

        /// True when a search is active, meaning we should show the overlay.
        pub active: Cell<bool>,

        /// Total number of search matches (null means unknown/none).
        pub search_total: Cell<Option<usize>>,

        /// Currently selected match index (null means none selected).
        pub search_selected: Cell<Option<usize>>,

        /// Target horizontal alignment for the overlay.
        pub halign_target: Cell<gtk4::Align>,

        /// Target vertical alignment for the overlay.
        pub valign_target: Cell<gtk4::Align>,
    }

    impl Default for SearchOverlay {
        fn default() -> Self {
            SearchOverlay {
                search_entry: TemplateChild::default(),
                active: Cell::new(false),
                search_total: Cell::new(None),
                search_selected: Cell::new(None),
                halign_target: Cell::new(gtk4::Align::End),
                valign_target: Cell::new(gtk4::Align::Start),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SearchOverlay {
        const NAME: &'static str = "GhosttySearchOverlay";
        type Type = super::SearchOverlay;
        type ParentType = libadwaita::Bin;

        fn class_init(klass: &mut Self::Class) {
            class::ensure_resources();
            klass.bind_template();

            // Template Callbacks
            // NOTE: The callbacks below ignore the first parameter
            // because they're shared with multiple widgets in the template.
            class::bind_template_callback(klass, "stop_search", |args| {
                let this: super::SearchOverlay = args.template();
                this.emit_by_name::<()>(signals::STOP_SEARCH, &[]);
                None
            });
            class::bind_template_callback(klass, "search_changed", |args| {
                // (entry: *gtk.SearchEntry, self)
                let entry: gtk4::SearchEntry = args.get(0);
                let this: super::SearchOverlay = args.template();
                let text = entry.text();
                this.emit_by_name::<()>(signals::SEARCH_CHANGED, &[&Some(text.as_str())]);
                None
            });
            class::bind_template_callback(klass, "match_label_closure", |args| {
                // (self, has_selected, selected, has_total, total)
                Some(
                    super::closure_match_label(
                        args.get(1),
                        args.get(2),
                        args.get(3),
                        args.get(4),
                    )
                    .to_value(),
                )
            });
            class::bind_template_callback(klass, "next_match", |args| {
                let this: super::SearchOverlay = args.template();
                this.emit_by_name::<()>(signals::NEXT_MATCH, &[]);
                None
            });
            class::bind_template_callback(klass, "previous_match", |args| {
                let this: super::SearchOverlay = args.template();
                this.emit_by_name::<()>(signals::PREVIOUS_MATCH, &[]);
                None
            });
            class::bind_template_callback(klass, "search_entry_key_pressed", |args| {
                // (controller, keyval: c_uint, keycode: c_uint, mods, self) -> c_int
                let this: super::SearchOverlay = args.template();
                let keyval: u32 = args.get(1);
                let mods: gdk::ModifierType = args.get(3);
                Some(this.search_entry_key_pressed(keyval, mods).to_value())
            });
            class::bind_template_callback(klass, "on_drag_end", |args| {
                // (gesture, offset_x: f64, offset_y: f64, self)
                let this: super::SearchOverlay = args.template();
                this.on_drag_end(args.get(1), args.get(2));
                None
            });
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for SearchOverlay {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPS: LazyLock<Vec<glib::ParamSpec>> = LazyLock::new(|| {
                vec![
                    glib::ParamSpecBoolean::builder(properties::ACTIVE).default_value(false).build(),
                    glib::ParamSpecUInt64::builder(properties::SEARCH_TOTAL)
                        .default_value(0)
                        .minimum(0)
                        .maximum(u64::MAX)
                        .read_only()
                        .build(),
                    glib::ParamSpecBoolean::builder(properties::HAS_SEARCH_TOTAL)
                        .default_value(false)
                        .read_only()
                        .build(),
                    glib::ParamSpecUInt64::builder(properties::SEARCH_SELECTED)
                        .default_value(0)
                        .minimum(0)
                        .maximum(u64::MAX)
                        .read_only()
                        .build(),
                    glib::ParamSpecBoolean::builder(properties::HAS_SEARCH_SELECTED)
                        .default_value(false)
                        .read_only()
                        .build(),
                    glib::ParamSpecEnum::builder_with_default(
                        properties::HALIGN_TARGET,
                        gtk4::Align::End,
                    )
                    .build(),
                    glib::ParamSpecEnum::builder_with_default(
                        properties::VALIGN_TARGET,
                        gtk4::Align::Start,
                    )
                    .build(),
                ]
            });
            PROPS.as_ref()
        }

        fn property(&self, _: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                properties::ACTIVE => self.active.get().to_value(),
                properties::SEARCH_TOTAL => {
                    (self.search_total.get().unwrap_or(0) as u64).to_value()
                }
                properties::HAS_SEARCH_TOTAL => self.search_total.get().is_some().to_value(),
                properties::SEARCH_SELECTED => {
                    (self.search_selected.get().unwrap_or(0) as u64).to_value()
                }
                properties::HAS_SEARCH_SELECTED => {
                    self.search_selected.get().is_some().to_value()
                }
                properties::HALIGN_TARGET => accessor::get_shallow(&self.halign_target),
                properties::VALIGN_TARGET => accessor::get_shallow(&self.valign_target),
                _ => unreachable!(),
            }
        }

        fn set_property(&self, _: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            match pspec.name() {
                properties::ACTIVE => self.obj().set_search_active(value.get().unwrap()),
                properties::HALIGN_TARGET => accessor::set_shallow(&self.halign_target, value),
                properties::VALIGN_TARGET => accessor::set_shallow(&self.valign_target, value),
                _ => unreachable!(),
            }
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: LazyLock<Vec<Signal>> = LazyLock::new(|| {
                vec![
                    Signal::builder(signals::STOP_SEARCH).build(),
                    Signal::builder(signals::SEARCH_CHANGED)
                        .param_types([String::static_type()])
                        .build(),
                    Signal::builder(signals::NEXT_MATCH).build(),
                    Signal::builder(signals::PREVIOUS_MATCH).build(),
                ]
            });
            SIGNALS.as_ref()
        }

        fn dispose(&self) {
            self.dispose_template();
        }
    }

    impl WidgetImpl for SearchOverlay {}
    impl BinImpl for SearchOverlay {}
}

glib::wrapper! {
    /// The overlay that shows the current size while a surface is resizing.
    /// This can be used generically to show pretty much anything with a
    /// disappearing overlay, but we have no other use at this point so it
    /// is named specifically for what it does.
    ///
    /// General usage:
    ///
    ///   1. Add it to an overlay
    ///   2. Set the label with `setLabel`
    ///   3. Schedule to show it with `schedule`
    ///
    /// Set any properties to change the behavior.
    pub struct SearchOverlay(ObjectSubclass<imp::SearchOverlay>)
        @extends libadwaita::Bin, gtk4::Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget;
}

impl Default for SearchOverlay {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl SearchOverlay {
    /// Grab focus on the search entry and select all text.
    pub fn grab_focus(&self) {
        let priv_ = self.imp();
        let _ = priv_.search_entry.grab_focus();

        // Select all text in the search entry field. -1 is distance from
        // the end, causing the entire text to be selected.
        priv_.search_entry.select_region(0, -1);
    }

    // Set active status, and update search on activation
    fn set_search_active(&self, active: bool) {
        let priv_ = self.imp();
        if !priv_.active.get() && active {
            let text = priv_.search_entry.text();
            self.emit_by_name::<()>(signals::SEARCH_CHANGED, &[&Some(text.as_str())]);
        }
        priv_.active.set(active);
    }

    // Set contents of search
    pub fn set_search_contents(&self, content: &str) {
        let priv_ = self.imp();
        priv_.search_entry.set_text(content);
        self.emit_by_name::<()>(signals::SEARCH_CHANGED, &[&Some(content)]);
    }

    /// Set the total number of search matches.
    pub fn set_search_total(&self, total: Option<usize>) {
        let priv_ = self.imp();
        let had_total = priv_.search_total.get().is_some();
        if priv_.search_total.get() == total {
            return;
        }
        priv_.search_total.set(total);
        self.notify(properties::SEARCH_TOTAL);
        if had_total != total.is_some() {
            self.notify(properties::HAS_SEARCH_TOTAL);
        }
    }

    /// Set the currently selected match index.
    pub fn set_search_selected(&self, selected: Option<usize>) {
        let priv_ = self.imp();
        let had_selected = priv_.search_selected.get().is_some();
        if priv_.search_selected.get() == selected {
            return;
        }
        priv_.search_selected.set(selected);
        self.notify(properties::SEARCH_SELECTED);
        if had_selected != selected.is_some() {
            self.notify(properties::HAS_SEARCH_SELECTED);
        }
    }

    /// `signals.@"search-changed".connect`
    pub fn connect_search_changed<F: Fn(&Self, Option<&str>) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_local(signals::SEARCH_CHANGED, false, move |values| {
            let this: Self = values[0].get().unwrap();
            let text: Option<String> = values[1].get().unwrap();
            f(&this, text.as_deref());
            None
        })
    }

    fn connect_simple<F: Fn(&Self) + 'static>(&self, name: &str, f: F) -> glib::SignalHandlerId {
        self.connect_local(name, false, move |values| {
            f(&values[0].get::<Self>().unwrap());
            None
        })
    }

    /// `signals.@"stop-search".connect`
    pub fn connect_stop_search<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_simple(signals::STOP_SEARCH, f)
    }

    /// `signals.@"next-match".connect`
    pub fn connect_next_match<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_simple(signals::NEXT_MATCH, f)
    }

    /// `signals.@"previous-match".connect`
    pub fn connect_previous_match<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_simple(signals::PREVIOUS_MATCH, f)
    }

    //---------------------------------------------------------------
    // Template callbacks

    fn search_entry_key_pressed(&self, keyval: u32, gtk_mods: gdk::ModifierType) -> bool {
        use glib::translate::IntoGlib;
        if keyval == gdk::Key::Return.into_glib() || keyval == gdk::Key::KP_Enter.into_glib() {
            if gtk_mods.contains(gdk::ModifierType::SHIFT_MASK) {
                self.emit_by_name::<()>(signals::PREVIOUS_MATCH, &[]);
            } else {
                self.emit_by_name::<()>(signals::NEXT_MATCH, &[]);
            }

            return true;
        }

        false
    }

    #[allow(deprecated)] // Zig uses getAllocatedWidth/Height too.
    fn on_drag_end(&self, offset_x: f64, offset_y: f64) {
        // On drag end, we want to move our halign/valign if we crossed
        // the midpoint on either axis. This lets the search overlay be
        // moved to different corners of the parent container.

        let priv_ = self.imp();
        let Some(parent) = self.parent() else { return };

        let parent_width = parent.allocated_width() as f64;
        let parent_height = parent.allocated_height() as f64;
        let self_width = self.allocated_width() as f64;
        let self_height = self.allocated_height() as f64;

        let self_x = if priv_.halign_target.get() == gtk4::Align::Start {
            0.0
        } else {
            parent_width - self_width
        };
        let self_y = if priv_.valign_target.get() == gtk4::Align::Start {
            0.0
        } else {
            parent_height - self_height
        };

        let new_x = self_x + offset_x + (self_width / 2.0);
        let new_y = self_y + offset_y + (self_height / 2.0);

        let new_halign =
            if new_x > parent_width / 2.0 { gtk4::Align::End } else { gtk4::Align::Start };
        let new_valign =
            if new_y > parent_height / 2.0 { gtk4::Align::End } else { gtk4::Align::Start };

        let mut changed = false;
        if new_halign != priv_.halign_target.get() {
            priv_.halign_target.set(new_halign);
            self.notify(properties::HALIGN_TARGET);
            changed = true;
        }
        if new_valign != priv_.valign_target.get() {
            priv_.valign_target.set(new_valign);
            self.notify(properties::VALIGN_TARGET);
            changed = true;
        }

        if changed {
            self.queue_resize();
        }
    }
}

fn closure_match_label(has_selected: bool, selected: u64, has_total: bool, total: u64) -> String {
    if !has_total || total == 0 {
        return "0/0".to_owned();
    }
    format!("{}/{}", if has_selected { selected + 1 } else { 0 }, total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apprt::gtk::ext::test_gtk;
    use std::cell::RefCell;
    use std::rc::Rc;
    use glib::translate::IntoGlib;

    #[test]
    fn blueprint_path() {
        assert_eq!(class::blueprint(BLUEPRINT), "/com/mitchellh/ghostty/ui/1.0/search-overlay.ui");
    }

    #[test]
    fn match_label() {
        assert_eq!(closure_match_label(false, 0, false, 0), "0/0");
        assert_eq!(closure_match_label(true, 2, true, 0), "0/0");
        assert_eq!(closure_match_label(true, 2, true, 5), "3/5");
        assert_eq!(closure_match_label(false, 2, true, 5), "0/5");
    }

    #[test]
    fn instantiate_and_signals() {
        test_gtk::run(|| {
            let o = SearchOverlay::default();
            let events = Rc::new(RefCell::new(Vec::<String>::new()));

            let e = events.clone();
            o.connect_search_changed(move |_, t| e.borrow_mut().push(format!("changed:{}", t.unwrap_or(""))));
            let e = events.clone();
            o.connect_next_match(move |_| e.borrow_mut().push("next".into()));
            let e = events.clone();
            o.connect_previous_match(move |_| e.borrow_mut().push("prev".into()));
            let e = events.clone();
            o.connect_stop_search(move |_| e.borrow_mut().push("stop".into()));

            o.set_search_contents("foo");
            o.set_property(properties::ACTIVE, true);
            assert!(o.property::<bool>(properties::ACTIVE));
            assert!(o.search_entry_key_pressed(gdk::Key::Return.into_glib(), gdk::ModifierType::empty()));
            assert!(o.search_entry_key_pressed(gdk::Key::KP_Enter.into_glib(), gdk::ModifierType::SHIFT_MASK));
            assert!(!o.search_entry_key_pressed(gdk::Key::a.into_glib(), gdk::ModifierType::empty()));
            o.imp().search_entry.emit_by_name::<()>("stop-search", &[]);

            let ev = events.borrow().clone();
            assert_eq!(ev[0], "changed:foo");
            assert!(ev.contains(&"changed:foo".to_owned()));
            assert!(ev.ends_with(&["next".into(), "prev".into(), "stop".into()]));

            o.set_search_total(Some(5));
            o.set_search_selected(Some(1));
            assert_eq!(o.property::<u64>(properties::SEARCH_TOTAL), 5);
            assert!(o.property::<bool>(properties::HAS_SEARCH_SELECTED));
            o.set_search_selected(None);
            assert!(!o.property::<bool>(properties::HAS_SEARCH_SELECTED));
            assert_eq!(o.property::<gtk4::Align>(properties::HALIGN_TARGET), gtk4::Align::End);
        });
    }
}
