//! Port of `src/apprt/gtk/class/resize_overlay.zig`.
//!
//! STATUS: ported
//!
//! The idle/timeout callbacks hold a weak reference to the overlay (Zig
//! passes an unowned pointer); the sources are removed in `dispose`
//! exactly as in Zig.
#![cfg(feature = "gtk")]

use std::cell::{Cell, RefCell};
use std::sync::LazyLock;
use std::time::Duration;

use glib::prelude::*;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use libadwaita::subclass::prelude::*;

use crate::apprt::gtk::class::{self, accessor, Blueprint};
use crate::apprt::gtk::ext;

const LOG: &str = "gtk_ghostty_resize_overlay";

/// The template of this class (Zig: `gresource.blueprint(...)`).
pub const BLUEPRINT: Blueprint = Blueprint { major: 1, minor: 0, name: "resize-overlay" };

/// Property names (Zig: `properties`).
pub mod properties {
    pub const DURATION: &str = "duration";
    pub const FIRST_DELAY: &str = "first-delay";
    pub const LABEL: &str = "label";
    pub const OVERLAY_HALIGN: &str = "overlay-halign";
    pub const OVERLAY_VALIGN: &str = "overlay-valign";
}

mod imp {
    use super::*;

    #[derive(gtk4::CompositeTemplate)]
    #[template(resource = "/com/mitchellh/ghostty/ui/1.0/resize-overlay.ui")]
    pub struct ResizeOverlay {
        /// The label with the text
        #[template_child]
        pub label: TemplateChild<gtk4::Label>,

        /// The text to set on the label when scheduled.
        pub label_text: RefCell<Option<String>>,

        /// The time that the overlay appears.
        pub duration: Cell<u32>,

        /// The first delay before any overlay is shown. Must be specified
        /// during construction otherwise it has no effect.
        pub first_delay: Cell<u32>,

        /// The idle source that we use to update the label.
        pub idler: RefCell<Option<glib::SourceId>>,

        /// The timer for dismissing the overlay.
        pub timer: RefCell<Option<glib::SourceId>>,

        /// The first delay timer.
        pub delay_timer: RefCell<Option<glib::SourceId>>,

        /// The alignment of the label
        pub halign: Cell<gtk4::Align>,
        pub valign: Cell<gtk4::Align>,
    }

    impl Default for ResizeOverlay {
        fn default() -> Self {
            // GObject zero-initializes the private data; the template sets
            // the real values before `instance_init` reads them.
            ResizeOverlay {
                label: TemplateChild::default(),
                label_text: RefCell::new(None),
                duration: Cell::new(0),
                first_delay: Cell::new(0),
                idler: RefCell::new(None),
                timer: RefCell::new(None),
                delay_timer: RefCell::new(None),
                halign: Cell::new(gtk4::Align::Fill),
                valign: Cell::new(gtk4::Align::Fill),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ResizeOverlay {
        const NAME: &'static str = "GhosttyResizeOverlay";
        type Type = super::ResizeOverlay;
        type ParentType = libadwaita::Bin;

        fn class_init(klass: &mut Self::Class) {
            class::ensure_resources();
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();

            // SAFETY: the instance and its private data are initialized
            // (this is what `init_template` itself does).
            let this = unsafe { obj.as_ref() };
            let priv_ = this.imp();
            if priv_.first_delay.get() > 0 {
                let weak = this.downgrade();
                let id = glib::timeout_add_local(
                    Duration::from_millis(priv_.first_delay.get() as u64),
                    move || {
                        if let Some(this) = weak.upgrade() {
                            this.on_delay_timer();
                        }
                        glib::ControlFlow::Break
                    },
                );
                priv_.delay_timer.replace(Some(id));
            }
        }
    }

    impl ObjectImpl for ResizeOverlay {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPS: LazyLock<Vec<glib::ParamSpec>> = LazyLock::new(|| {
                vec![
                    glib::ParamSpecUInt::builder(properties::DURATION)
                        .default_value(750)
                        .minimum(250)
                        .maximum(u32::MAX)
                        .build(),
                    glib::ParamSpecString::builder(properties::LABEL).build(),
                    glib::ParamSpecUInt::builder(properties::FIRST_DELAY)
                        .default_value(250)
                        .minimum(250)
                        .maximum(u32::MAX)
                        .build(),
                    glib::ParamSpecEnum::builder_with_default(
                        properties::OVERLAY_HALIGN,
                        gtk4::Align::Center,
                    )
                    .build(),
                    glib::ParamSpecEnum::builder_with_default(
                        properties::OVERLAY_VALIGN,
                        gtk4::Align::Center,
                    )
                    .build(),
                ]
            });
            PROPS.as_ref()
        }

        fn property(&self, _: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                properties::DURATION => accessor::get_shallow(&self.duration),
                properties::LABEL => accessor::get(&self.label_text),
                properties::FIRST_DELAY => accessor::get_shallow(&self.first_delay),
                properties::OVERLAY_HALIGN => accessor::get_shallow(&self.halign),
                properties::OVERLAY_VALIGN => accessor::get_shallow(&self.valign),
                _ => unreachable!(),
            }
        }

        fn set_property(&self, _: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            match pspec.name() {
                properties::DURATION => accessor::set_shallow(&self.duration, value),
                properties::LABEL => accessor::set(&self.label_text, value),
                properties::FIRST_DELAY => accessor::set_shallow(&self.first_delay, value),
                properties::OVERLAY_HALIGN => accessor::set_shallow(&self.halign, value),
                properties::OVERLAY_VALIGN => accessor::set_shallow(&self.valign, value),
                _ => unreachable!(),
            }
        }

        fn dispose(&self) {
            if let Some(v) = self.idler.take() {
                if !ext::source_remove(v) {
                    log::warn!(target: LOG, "unable to remove resize overlay idler");
                }
            }
            if let Some(v) = self.timer.take() {
                if !ext::source_remove(v) {
                    log::warn!(target: LOG, "unable to remove resize overlay timer");
                }
            }
            if let Some(v) = self.delay_timer.take() {
                if !ext::source_remove(v) {
                    log::warn!(target: LOG, "unable to remove resize overlay delay timer");
                }
            }

            self.dispose_template();
        }
    }

    impl WidgetImpl for ResizeOverlay {}
    impl BinImpl for ResizeOverlay {}
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
    pub struct ResizeOverlay(ObjectSubclass<imp::ResizeOverlay>)
        @extends libadwaita::Bin, gtk4::Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget;
}

impl Default for ResizeOverlay {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl ResizeOverlay {
    /// Set the label for the overlay. This will not show the
    /// overlay if it is currently hidden; you must call schedule.
    pub fn set_label(&self, label: Option<&str>) {
        self.imp().label_text.replace(label.map(str::to_owned));
        self.notify(properties::LABEL);
    }

    /// Schedule the overlay to be shown. To avoid flickering during
    /// resizes we schedule the overlay to be shown on the next idle tick.
    pub fn schedule(&self) {
        let priv_ = self.imp();

        // If we have a delay timer then we're not showing anything
        // yet so do nothing.
        if priv_.delay_timer.borrow().is_some() {
            return;
        }

        // When updating a widget, wait until GTK is "idle", i.e. not in the middle
        // of doing any other updates. Since we are called in the middle of resizing
        // GTK is doing a lot of work rearranging all of the widgets. Not doing this
        // results in a lot of warnings from GTK and _horrible_ flickering of the
        // resize overlay.
        if priv_.idler.borrow().is_some() {
            return;
        }
        let weak = self.downgrade();
        let id = glib::idle_add_local(move || {
            if let Some(this) = weak.upgrade() {
                this.on_idle();
            }
            glib::ControlFlow::Break
        });
        priv_.idler.replace(Some(id));
    }

    fn on_idle(&self) {
        let priv_ = self.imp();

        // No matter what our idler is complete with this callback
        priv_.idler.replace(None);

        // Cancel our previous show timer no matter what.
        if let Some(timer) = priv_.timer.take() {
            if !ext::source_remove(timer) {
                log::warn!(target: LOG, "unable to remove size overlay timer");
            }
        }

        // If we have a label to show, show ourselves. If we don't have
        // label text, then hide our label.
        let text = priv_.label_text.borrow().clone();
        let Some(text) = text else {
            self.set_visible(false);
            return;
        };

        // Set our label and show it.
        priv_.label.set_label(&text);
        self.set_visible(true);

        // Setup the new timer to hide ourselves after the delay.
        let weak = self.downgrade();
        let id = glib::timeout_add_local(
            Duration::from_millis(priv_.duration.get() as u64),
            move || {
                if let Some(this) = weak.upgrade() {
                    this.on_timer();
                }
                glib::ControlFlow::Break
            },
        );
        priv_.timer.replace(Some(id));
    }

    fn on_timer(&self) {
        self.imp().timer.replace(None);
        self.set_visible(false);
    }

    fn on_delay_timer(&self) {
        self.imp().delay_timer.replace(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apprt::gtk::ext::test_gtk;

    #[test]
    fn blueprint_path() {
        assert_eq!(class::blueprint(BLUEPRINT), "/com/mitchellh/ghostty/ui/1.0/resize-overlay.ui");
    }

    #[test]
    fn instantiate_and_schedule() {
        test_gtk::run(|| {
            let o = ResizeOverlay::default();
            // Template values.
            assert_eq!(o.property::<u32>(properties::DURATION), 750);
            assert_eq!(o.property::<u32>(properties::FIRST_DELAY), 250);
            assert!(!o.is_visible());

            // The first delay is pending, so scheduling does nothing.
            assert!(o.imp().delay_timer.borrow().is_some());
            o.set_label(Some("80 x 24"));
            o.schedule();
            assert!(o.imp().idler.borrow().is_none());

            // Once the delay passed, scheduling shows the label on idle.
            o.on_delay_timer();
            o.schedule();
            assert!(o.imp().idler.borrow().is_some());
            o.on_idle();
            assert!(o.is_visible());
            assert_eq!(o.imp().label.label().as_str(), "80 x 24");
            assert!(o.imp().timer.borrow().is_some());

            o.on_timer();
            assert!(!o.is_visible());

            // Without a label the overlay hides.
            o.set_label(None);
            o.schedule();
            o.on_idle();
            assert!(!o.is_visible());
            // SAFETY: `o` is not used after being disposed.
            unsafe { o.run_dispose() };
        });
    }
}
