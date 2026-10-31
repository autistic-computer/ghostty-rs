//! Port of `src/apprt/gtk/class/tab.zig`.
//!
//! STATUS: ported
#![cfg(feature = "gtk")]

use std::cell::RefCell;
use std::sync::LazyLock;

use glib::prelude::*;
use glib::subclass::Signal;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;

use crate::apprt::action::CloseTabMode;
use crate::apprt::gtk::class::overrides::Overrides;
use crate::apprt::gtk::class::title_dialog::{self, TitleDialog};
use crate::apprt::gtk::class::{self, accessor, Blueprint, Config};
use crate::apprt::gtk::ext;
use crate::apprt::gtk::ext::actions::Action;
use crate::apprt::surface::NewSurfaceContext;
use crate::datastruct::split_tree::Direction;

use crate::apprt::gtk::class::application::Application;
use crate::apprt::gtk::class::split_tree::SplitTree;
use crate::apprt::gtk::class::surface::{Surface, Tree as SurfaceTree};
use crate::surface::Surface as CoreSurface;

const LOG: &str = "gtk_ghostty_window";

/// The template of this class (Zig: `gresource.blueprint(...)`).
pub const BLUEPRINT: Blueprint = Blueprint { major: 1, minor: 0, name: "tab" };

/// Property names (Zig: `properties`).
pub mod properties {
    /// The active surface is the surface that should be receiving all
    /// surface-targeted actions. This is usually the focused surface,
    /// but may also not be focused if the user has selected a non-surface
    /// widget.
    pub const ACTIVE_SURFACE: &str = "active-surface";
    pub const CONFIG: &str = "config";
    pub const SPLIT_TREE: &str = "split-tree";
    pub const SURFACE_TREE: &str = "surface-tree";
    pub const TOOLTIP: &str = "tooltip";
    pub const TITLE: &str = "title";
    pub const TITLE_OVERRIDE: &str = "title-override";
}

/// Signal names (Zig: `signals`).
pub mod signals {
    /// Emitted whenever the tab would like to be closed.
    pub const CLOSE_REQUEST: &str = "close-request";
}

mod imp {
    use super::*;

    #[derive(Default, gtk4::CompositeTemplate)]
    #[template(resource = "/com/mitchellh/ghostty/ui/1.0/tab.ui")]
    pub struct Tab {
        /// The configuration that this surface is using.
        pub config: RefCell<Option<Config>>,

        /// The title of this tab. This is usually bound to the active surface.
        pub title: RefCell<Option<String>>,

        /// The manually overridden title.
        pub title_override: RefCell<Option<String>>,

        /// The tooltip of this tab. This is usually bound to the active surface.
        pub tooltip: RefCell<Option<String>>,

        // Template bindings
        #[template_child]
        pub split_tree: TemplateChild<SplitTree>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Tab {
        const NAME: &'static str = "GhosttyTab";
        type Type = super::Tab;
        type ParentType = gtk4::Box;

        fn class_init(klass: &mut Self::Class) {
            SplitTree::ensure_type();
            Surface::ensure_type();
            Config::ensure_type();
            class::ensure_resources();
            klass.bind_template();

            // Template Callbacks
            class::bind_template_callback(klass, "computed_title", |args| {
                // (self, config, terminal, surface_override, tab_override,
                //  zoomed, bell_ringing)
                let config: Option<Config> = args.get(1);
                let terminal: Option<String> = args.get(2);
                let surface_override: Option<String> = args.get(3);
                let tab_override: Option<String> = args.get(4);
                let zoomed: bool = args.get(5);
                let bell_ringing: bool = args.get(6);
                Some(
                    super::closure_computed_title(
                        config.as_ref(),
                        terminal.as_deref(),
                        surface_override.as_deref(),
                        tab_override.as_deref(),
                        zoomed,
                        bell_ringing,
                    )
                    .to_value(),
                )
            });
            class::bind_template_callback(klass, "notify_active_surface", |args| {
                let this: super::Tab = args.template();
                this.prop_active_surface();
                None
            });
            class::bind_template_callback(klass, "notify_tree", |args| {
                let this: super::Tab = args.template();
                this.prop_split_tree();
                None
            });
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();

            // Init our actions
            // SAFETY: the instance is initialized (as in `init_template`).
            unsafe { obj.as_ref() }.init_action_map();
        }
    }

    impl ObjectImpl for Tab {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPS: LazyLock<Vec<glib::ParamSpec>> = LazyLock::new(|| {
                vec![
                    glib::ParamSpecObject::builder::<Surface>(properties::ACTIVE_SURFACE)
                        .read_only()
                        .build(),
                    glib::ParamSpecObject::builder::<Config>(properties::CONFIG).build(),
                    glib::ParamSpecObject::builder::<SplitTree>(properties::SPLIT_TREE)
                        .read_only()
                        .build(),
                    glib::ParamSpecBoxed::builder::<SurfaceTree>(properties::SURFACE_TREE)
                        .read_only()
                        .build(),
                    glib::ParamSpecString::builder(properties::TITLE).build(),
                    glib::ParamSpecString::builder(properties::TITLE_OVERRIDE).build(),
                    glib::ParamSpecString::builder(properties::TOOLTIP).build(),
                ]
            });
            PROPS.as_ref()
        }

        fn property(&self, _: usize, pspec: &glib::ParamSpec) -> glib::Value {
            let obj = self.obj();
            match pspec.name() {
                properties::ACTIVE_SURFACE => obj.get_active_surface().to_value(),
                properties::CONFIG => accessor::get(&self.config),
                properties::SPLIT_TREE => obj.get_split_tree().to_value(),
                properties::SURFACE_TREE => obj.get_surface_tree().to_value(),
                properties::TITLE => accessor::get(&self.title),
                properties::TITLE_OVERRIDE => accessor::get(&self.title_override),
                properties::TOOLTIP => accessor::get(&self.tooltip),
                _ => unreachable!(),
            }
        }

        fn set_property(&self, _: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            match pspec.name() {
                properties::CONFIG => accessor::set(&self.config, value),
                properties::TITLE => accessor::set(&self.title, value),
                properties::TITLE_OVERRIDE => accessor::set(&self.title_override, value),
                properties::TOOLTIP => accessor::set(&self.tooltip, value),
                _ => unreachable!(),
            }
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: LazyLock<Vec<Signal>> =
                LazyLock::new(|| vec![Signal::builder(signals::CLOSE_REQUEST).build()]);
            SIGNALS.as_ref()
        }

        fn dispose(&self) {
            self.config.replace(None);
            self.dispose_template();
        }

        // finalize: the strings are dropped with the private struct.
    }

    impl WidgetImpl for Tab {}
    impl BoxImpl for Tab {}
}

glib::wrapper! {
    pub struct Tab(ObjectSubclass<imp::Tab>)
        @extends gtk4::Box, gtk4::Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget,
            gtk4::Orientable;
}

impl Tab {
    /// Set the parent of this tab page. This only affects the first surface
    /// ever created for a tab. If a surface was already created this does
    /// nothing.
    pub fn set_parent(&self, parent: &CoreSurface) {
        self.set_parent_with_context(parent, NewSurfaceContext::Tab);
    }

    pub fn set_parent_with_context(&self, parent: &CoreSurface, context: NewSurfaceContext) {
        if let Some(surface) = self.get_active_surface() {
            surface.set_parent(parent, context);
        }
    }

    pub fn new(config: Option<&Config>, overrides: &Overrides) -> Tab {
        let tab: Tab = glib::Object::new();

        let priv_ = tab.imp();

        if let Some(c) = config {
            priv_.config.replace(Some(c.clone()));
        }

        // If our configuration is null then we get the configuration
        // from the application.
        if priv_.config.borrow().is_none() {
            let app = Application::default();
            priv_.config.replace(app.get_config());
        }

        tab.notify(properties::CONFIG);

        // Create our initial surface in the split tree.
        if let Err(err) = priv_.split_tree.new_split(Direction::Right, None, overrides) {
            match err {
                // TODO: We should make our "no surfaces" state more aesthetically
                // pleasing and show something like an "Oops, something went wrong"
                // message. For now, this is incredibly unlikely.
                crate::datastruct::split_tree::Error::OutOfMemory => panic!("oom"),
            }
        }

        tab
    }

    fn init_action_map(&self) {
        let s_param_type = glib::VariantTy::STRING;

        let actions = [
            Action::<Tab>::init("close", action_close, Some(s_param_type)),
            Action::init("ring-bell", action_ring_bell, None),
            Action::init("next-page", action_next_page, None),
            Action::init("previous-page", action_previous_page, None),
            Action::init("prompt-tab-title", action_prompt_tab_title, None),
        ];

        let _ = ext::actions::add_as_group(self, "tab", &actions);
    }

    //---------------------------------------------------------------
    // Properties

    /// Overridden title. This will be generally be shown over the title
    /// unless this is unset (null).
    pub fn set_title_override(&self, title: Option<&str>) {
        self.imp().title_override.replace(title.map(str::to_owned));
        self.notify(properties::TITLE_OVERRIDE);
    }

    fn title_dialog_set(&self, title: &str) {
        self.set_title_override(if title.is_empty() { None } else { Some(title) });
    }

    pub fn prompt_tab_title(&self) {
        let priv_ = self.imp();
        let initial = priv_
            .title_override
            .borrow()
            .clone()
            .or_else(|| priv_.title.borrow().clone());
        let dialog = TitleDialog::new(title_dialog::Target::Tab, initial.as_deref());
        let weak = self.downgrade();
        dialog.connect_set(move |_, title| {
            if let Some(this) = weak.upgrade() {
                this.title_dialog_set(title);
            }
        });

        dialog.present(self.upcast_ref());
    }

    /// Get the currently active surface. See the "active-surface" property.
    pub fn get_active_surface(&self) -> Option<Surface> {
        self.get_split_tree().get_active_surface()
    }

    /// Get the surface tree of this tab.
    pub fn get_surface_tree(&self) -> Option<SurfaceTree> {
        self.imp().split_tree.get_tree()
    }

    /// Get the split tree widget that is in this tab.
    pub fn get_split_tree(&self) -> SplitTree {
        self.imp().split_tree.get()
    }

    /// Returns true if this tab needs confirmation before quitting based
    /// on the various Ghostty configurations.
    pub fn get_needs_confirm_quit(&self) -> bool {
        let tree = self.get_split_tree();
        tree.get_needs_confirm_quit()
    }

    /// Get the tab view holding this tab, if any.
    fn get_tab_view(&self) -> Option<libadwaita::TabView> {
        ext::get_ancestor::<libadwaita::TabView>(self)
    }

    /// Get the tab page holding this tab, if any.
    fn get_tab_page(&self) -> Option<libadwaita::TabPage> {
        let tab_view = self.get_tab_view()?;
        Some(tab_view.page(self))
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

    fn prop_split_tree(&self) {
        self.notify(properties::SURFACE_TREE);

        // If our tree is empty we close the tab.
        let empty = self.get_surface_tree().is_none_or(|tree| tree.0.is_empty());
        if empty {
            self.emit_by_name::<()>(signals::CLOSE_REQUEST, &[]);
        }
    }

    fn prop_active_surface(&self) {
        self.notify(properties::ACTIVE_SURFACE);
    }
}

fn action_close(_: &gio::SimpleAction, param: Option<&glib::Variant>, this: &Tab) {
    let Some(param) = param else {
        log::warn!(target: LOG, "tab.close-tab called without a parameter");
        return;
    };

    let str = param.str();

    let Some(tab_view) = this.get_tab_view() else { return };
    let page = tab_view.page(this);

    let Some(str) = str else {
        log::warn!(target: LOG, "invalid mode provided to tab.close-tab");
        return;
    };
    let mode = match str {
        "this" => CloseTabMode::This,
        "other" => CloseTabMode::Other,
        "right" => CloseTabMode::Right,
        _ => {
            // Need to be defensive here since actions can be triggered externally.
            log::warn!(target: LOG, "invalid mode provided to tab.close-tab: {str}");
            return;
        }
    };

    // Delegate to our parent to handle this, since this will emit
    // a close-page signal that the parent can intercept.
    match mode {
        CloseTabMode::This => tab_view.close_page(&page),
        CloseTabMode::Other => tab_view.close_other_pages(&page),
        CloseTabMode::Right => tab_view.close_pages_after(&page),
    }
}

fn action_prompt_tab_title(_: &gio::SimpleAction, _: Option<&glib::Variant>, this: &Tab) {
    this.prompt_tab_title();
}

fn action_ring_bell(_: &gio::SimpleAction, _: Option<&glib::Variant>, this: &Tab) {
    // Future note: I actually don't like this logic living here at all.
    // I think a better approach will be for the ring bell action to
    // specify its sending surface and then do all this in the window.

    // If the page is selected already we don't mark it as needing
    // attention. We only want to mark unfocused pages. This will then
    // clear when the page is selected.
    let Some(page) = this.get_tab_page() else { return };
    if page.is_selected() {
        return;
    }
    page.set_needs_attention(true);
}

/// Select the next tab page.
fn action_next_page(_: &gio::SimpleAction, _: Option<&glib::Variant>, this: &Tab) {
    let Some(tab_view) = this.get_tab_view() else { return };
    let _ = tab_view.select_next_page();
}

/// Select the previous tab page.
fn action_previous_page(_: &gio::SimpleAction, _: Option<&glib::Variant>, this: &Tab) {
    let Some(tab_view) = this.get_tab_view() else { return };
    let _ = tab_view.select_previous_page();
}

fn closure_computed_title(
    config: Option<&Config>,
    terminal: Option<&str>,
    surface_override: Option<&str>,
    tab_override: Option<&str>,
    zoomed: bool,
    bell_ringing: bool,
) -> String {
    // Our plain title is the manually tab overridden title if it exists,
    // otherwise the overridden title if it exists, otherwise
    // the terminal title if it exists, otherwise a default string.
    let config_title: Option<String> =
        config.and_then(|c| c.get().title.as_ref().map(|t| t.as_str().to_owned()));
    let plain: &str = tab_override
        .or(surface_override)
        .or(terminal)
        .or(config_title.as_deref())
        .unwrap_or("Ghostty");

    // We don't need a config in every case, but if we don't have a config
    // let's just assume something went terribly wrong and use our
    // default title. Its easier then guarding on the config existing
    // in every case for something so unlikely.
    let Some(config) = config else {
        log::warn!(target: LOG, "config unavailable for computed title, likely bug");
        return plain.to_owned();
    };
    let config = config.get();

    let mut buf = String::new();

    // If our bell is ringing, then we prefix the bell icon to the title.
    if bell_ringing && config.bell_features.title {
        buf.push_str("🔔 ");
    }

    // If we're zoomed, prefix with the magnifying glass emoji.
    if zoomed {
        buf.push_str("🔍 ");
    }

    buf.push_str(plain);
    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apprt::gtk::ext::test_gtk;
    use crate::config::config::Config as CoreConfig;
    use std::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn blueprint_path() {
        assert_eq!(class::blueprint(BLUEPRINT), "/com/mitchellh/ghostty/ui/1.0/tab.ui");
    }

    #[test]
    fn computed_title() {
        assert_eq!(closure_computed_title(None, None, None, None, false, false), "Ghostty");
        assert_eq!(closure_computed_title(None, Some("t"), None, None, true, true), "t");

        let mut core = CoreConfig::default();
        let config = Config::new(&core);
        assert_eq!(
            closure_computed_title(Some(&config), Some("t"), Some("s"), None, false, false),
            "s"
        );
        assert_eq!(
            closure_computed_title(Some(&config), Some("t"), Some("s"), Some("x"), true, true),
            "🔔 🔍 x"
        );

        core.title = Some("cfg".into());
        core.bell_features.title = false;
        let config = Config::new(&core);
        assert_eq!(
            closure_computed_title(Some(&config), None, None, None, false, true),
            "cfg"
        );
    }

    #[test]
    fn instantiate() {
        test_gtk::run(|| {
            let config = Config::new(&CoreConfig::default());
            let tab = Tab::new(Some(&config), &Overrides {
                title: Some("hello".into()),
                ..Overrides::NONE
            });
            assert_eq!(tab.property::<Option<Config>>(properties::CONFIG).as_ref(), Some(&config));

            // The initial surface was created.
            let surface = tab.get_active_surface().unwrap();
            assert_eq!(
                tab.property::<Option<Surface>>(properties::ACTIVE_SURFACE).as_ref(),
                Some(&surface)
            );
            assert!(tab.get_surface_tree().is_some());
            assert!(!tab.get_needs_confirm_quit());

            // The title is computed from the surface.
            assert_eq!(tab.property::<Option<String>>(properties::TITLE).as_deref(), Some("hello"));
            tab.set_title_override(Some("override"));
            assert_eq!(
                tab.property::<Option<String>>(properties::TITLE).as_deref(),
                Some("override")
            );
            tab.title_dialog_set("");
            assert_eq!(tab.property::<Option<String>>(properties::TITLE).as_deref(), Some("hello"));

            // Emptying the tree requests a close.
            let closed = Rc::new(Cell::new(false));
            let c = closed.clone();
            tab.connect_close_request(move |_| c.set(true));
            tab.get_split_tree().set_tree(None);
            assert!(closed.get());
        });
    }

    #[test]
    fn tab_view_actions() {
        test_gtk::run(|| {
            let config = Config::new(&CoreConfig::default());
            let view = libadwaita::TabView::new();
            let tabs: Vec<Tab> = (0..3).map(|_| Tab::new(Some(&config), &Overrides::NONE)).collect();
            for t in &tabs {
                view.append(t);
            }
            view.set_selected_page(&view.page(&tabs[0]));

            // ring-bell marks only unselected pages.
            tabs[0].activate_action("tab.ring-bell", None).unwrap();
            assert!(!view.page(&tabs[0]).needs_attention());
            tabs[1].activate_action("tab.ring-bell", None).unwrap();
            assert!(view.page(&tabs[1]).needs_attention());

            tabs[0].activate_action("tab.next-page", None).unwrap();
            assert_eq!(view.selected_page(), Some(view.page(&tabs[1])));
            tabs[1].activate_action("tab.previous-page", None).unwrap();
            assert_eq!(view.selected_page(), Some(view.page(&tabs[0])));

            // Invalid modes are ignored.
            tabs[0].activate_action("tab.close", Some(&"bogus".to_variant())).unwrap();
            assert_eq!(view.n_pages(), 3);
            tabs[1].activate_action("tab.close", Some(&"right".to_variant())).unwrap();
            assert_eq!(view.n_pages(), 2);
            tabs[0].activate_action("tab.close", Some(&"this".to_variant())).unwrap();
            assert_eq!(view.n_pages(), 1);
        });
    }
}
