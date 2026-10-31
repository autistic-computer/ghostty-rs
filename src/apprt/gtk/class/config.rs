//! Port of `src/apprt/gtk/class/config.zig`.
//!
//! STATUS: ported
//!
//! The wrapped configuration lives in a `RefCell`, so [`Config::get`] and
//! [`Config::get_mut`] return `Ref`/`RefMut` guards instead of raw pointers.
#![cfg(feature = "gtk")]

use std::cell::{Ref, RefCell, RefMut};
use std::sync::LazyLock;

use glib::prelude::*;
use glib::subclass::prelude::*;
use gtk4::prelude::*;

use crate::config::config::Config as CoreConfig;

const LOG: &str = "gtk_ghostty_config";

/// Property names (Zig: `Config.properties`).
pub mod properties {
    pub const DIAGNOSTICS_BUFFER: &str = "diagnostics-buffer";
    pub const HAS_DIAGNOSTICS: &str = "has-diagnostics";
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Config {
        /// `Private.config`. `None` only between instance init and
        /// [`super::Config::new`].
        pub config: RefCell<Option<CoreConfig>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Config {
        const NAME: &'static str = "GhosttyConfig";
        type Type = super::Config;
        type ParentType = glib::Object;
    }

    impl ObjectImpl for Config {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPS: LazyLock<Vec<glib::ParamSpec>> = LazyLock::new(|| {
                vec![
                    glib::ParamSpecObject::builder::<gtk4::TextBuffer>(
                        properties::DIAGNOSTICS_BUFFER,
                    )
                    .read_only()
                    .build(),
                    glib::ParamSpecBoolean::builder(properties::HAS_DIAGNOSTICS)
                        .default_value(false)
                        .read_only()
                        .build(),
                ]
            });
            PROPS.as_ref()
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            let obj = self.obj();
            match pspec.name() {
                properties::DIAGNOSTICS_BUFFER => obj.diagnostics_buffer().to_value(),
                properties::HAS_DIAGNOSTICS => obj.has_diagnostics().to_value(),
                _ => unreachable!(),
            }
        }

        // finalize: the core config is dropped with the private data.
    }
}

glib::wrapper! {
    /// Wraps a `Ghostty.Config` object in a GObject so it can be reference
    /// counted. When this object is freed, the underlying config is also freed.
    ///
    /// It is highly recommended to NOT take a reference to this object,
    /// since configuration takes up a lot of memory (relatively). Instead,
    /// receivers of this should usually create a `DerivedConfig` struct from
    /// this, copy any memory they require, and own that structure instead.
    ///
    /// This can also expose helpers to access configuration in ways that
    /// may be more ergonomic to GTK primitives.
    pub struct Config(ObjectSubclass<imp::Config>);
}

impl Config {
    /// Create a new GhosttyConfig from a loaded configuration.
    ///
    /// This clones the given configuration, so it is safe for the
    /// caller to free the original configuration after this call.
    pub fn new(config: &CoreConfig) -> Config {
        let this: Config = glib::Object::new();
        this.imp().config.replace(Some(config.clone()));
        this
    }

    /// Get the wrapped configuration. It's unsafe to store this or access
    /// it in any way that may live beyond the lifetime of this object.
    pub fn get(&self) -> Ref<'_, CoreConfig> {
        Ref::map(self.imp().config.borrow(), |c| {
            c.as_ref().expect("GhosttyConfig used before Config::new")
        })
    }

    /// Get the mutable configuration. This is usually NOT recommended
    /// because any changes to the config won't be propagated to anyone
    /// with a reference to this object. If you know what you're doing, then
    /// you can use this.
    pub fn get_mut(&self) -> RefMut<'_, CoreConfig> {
        RefMut::map(self.imp().config.borrow_mut(), |c| {
            c.as_mut().expect("GhosttyConfig used before Config::new")
        })
    }

    /// Returns whether this configuration has any diagnostics.
    pub fn has_diagnostics(&self) -> bool {
        let config = self.get();
        !config._diagnostics.empty()
    }

    /// Reads the diagnostics of this configuration as a TextBuffer,
    /// or returns null if there are no diagnostics.
    pub fn diagnostics_buffer(&self) -> Option<gtk4::TextBuffer> {
        let config = self.get();
        if config._diagnostics.empty() {
            return None;
        }

        let text_buf = gtk4::TextBuffer::new(None);

        // Zig formats each diagnostic into a fixed 4095-byte buffer and
        // skips the ones that don't fit.
        const BUF_LEN: usize = 4095;
        let mut buf = String::with_capacity(BUF_LEN);
        for diag in config._diagnostics.items() {
            buf.clear();
            let result = diag.format(&mut buf);
            if let Err(err) = result {
                log::warn!(target: LOG, "error writing diagnostic to buffer err={err:?}");
                continue;
            }
            if buf.len() > BUF_LEN {
                log::warn!(target: LOG, "error writing diagnostic to buffer err=WriteFailed");
                continue;
            }

            text_buf.insert_at_cursor(&buf);
            text_buf.insert_at_cursor("\n");
        }

        Some(text_buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apprt::gtk::ext::test_gtk;
    use crate::cli::diagnostics::Diagnostic;

    // This test verifies our memory management works as expected. Since
    // we use the testing allocator any leaks are detected.
    #[test]
    fn ghostty_config() {
        let config = CoreConfig::default();
        let obj = Config::new(&config);
        drop(config);
        assert!(!obj.has_diagnostics());
        drop(obj);
    }

    #[test]
    fn ghostty_config_diagnostics() {
        test_gtk::run(|| {
            let mut config = CoreConfig::default();
            let obj = Config::new(&config);
            assert!(obj.diagnostics_buffer().is_none());
            assert!(!obj.property::<bool>(properties::HAS_DIAGNOSTICS));

            config._diagnostics.append(Diagnostic {
                message: "boom".into(),
                ..Default::default()
            });
            let obj = Config::new(&config);
            assert!(obj.property::<bool>(properties::HAS_DIAGNOSTICS));
            let buf = obj
                .property::<Option<gtk4::TextBuffer>>(properties::DIAGNOSTICS_BUFFER)
                .unwrap();
            let (start, end) = buf.bounds();
            assert_eq!(buf.text(&start, &end, false).as_str(), "boom\n");
        });
    }
}
