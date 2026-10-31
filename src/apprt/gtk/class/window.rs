//! Port of `src/apprt/gtk/class/window.zig`.
//!
//! STATUS: ported
//!
//! Rust notes:
//! * The windowing protocol state holds the window through [`WindowRef`], a
//!   weak reference implementing `winproto::ApprtWindow` (Zig hands it the
//!   window pointer). `get_config` hands the winproto a snapshot of the
//!   window's configuration.
//! * The `titlebar-style` property uses the GObject enum mirror
//!   [`TitlebarStyle`] (`GhosttyGtkTitlebarStyle`).
//! * The surface/tab signal handlers are tracked by id and disconnected by
//!   id (Zig disconnects everything with the window as user data).
#![cfg(feature = "gtk")]

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::LazyLock;

use glib::prelude::*;
use gtk4::gdk;
use gtk4::gio;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use libadwaita::prelude::*;
use libadwaita::subclass::prelude::*;

use crate::apprt::action::ResizeWindow;
use crate::apprt::gtk::class::application::Application;
use crate::apprt::gtk::class::close_confirmation_dialog::{self, CloseConfirmationDialog};
use crate::apprt::gtk::class::command_palette::CommandPalette;
use crate::apprt::gtk::class::debug_warning::DebugWarning;
use crate::apprt::gtk::class::overrides::Overrides;
use crate::apprt::gtk::class::split_tree::SplitTree;
use crate::apprt::gtk::class::surface::{self as surface_class, ClipboardType, Surface, Tree};
use crate::apprt::gtk::class::tab::Tab;
use crate::apprt::gtk::class::title_dialog::{self, TitleDialog};
use crate::apprt::gtk::class::{self, accessor, Blueprint, Config};
use crate::apprt::gtk::ext;
use crate::apprt::gtk::ext::actions::Action as GtkAction;
use crate::apprt::gtk::winproto;
use crate::apprt::surface::NewSurfaceContext;
use crate::apprt::Clipboard;
use crate::build_config;
use crate::config::config::{
    Config as CoreConfig, GtkTabsLocation, GtkTitlebarStyle, GtkToolbarStyle, WindowDecoration,
    WindowNewTabPosition, WindowShowTabBar, WindowSubtitle,
};
use crate::input::binding::{
    Action as BindingAction, CloseTabMode, CopyToClipboard, SplitDirection,
};
use crate::surface::Surface as CoreSurface;

const LOG: &str = "gtk_ghostty_window";

/// The template of this class (Zig: `gresource.blueprint(...)`).
pub const BLUEPRINT: Blueprint = Blueprint { major: 1, minor: 4, name: "window" };

/// Property names (Zig: `properties`).
pub mod properties {
    /// The active surface is the focus that should be receiving all
    /// surface-targeted actions. This is usually the focused surface,
    /// but may also not be focused if the user has selected a non-surface
    /// widget.
    pub const ACTIVE_SURFACE: &str = "active-surface";
    pub const CONFIG: &str = "config";
    pub const DEBUG: &str = "debug";
    pub const TITLEBAR_STYLE: &str = "titlebar-style";
    pub const HEADERBAR_VISIBLE: &str = "headerbar-visible";
    pub const QUICK_TERMINAL: &str = "quick-terminal";
    pub const TABS_AUTOHIDE: &str = "tabs-autohide";
    pub const TABS_INVERTED: &str = "tabs-inverted";
    pub const TABS_WIDE: &str = "tabs-wide";
    pub const TABS_VISIBLE: &str = "tabs-visible";
    pub const TOOLBAR_STYLE: &str = "toolbar-style";
    pub const TITLE_OVERRIDE: &str = "title-override";
}

/// `Config.GtkTitlebarStyle` as the enum type `GhosttyGtkTitlebarStyle`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, glib::Enum)]
#[enum_type(name = "GhosttyGtkTitlebarStyle")]
pub enum TitlebarStyle {
    #[default]
    Native,
    Tabs,
}

impl From<GtkTitlebarStyle> for TitlebarStyle {
    fn from(v: GtkTitlebarStyle) -> Self {
        match v {
            GtkTitlebarStyle::Native => TitlebarStyle::Native,
            GtkTitlebarStyle::Tabs => TitlebarStyle::Tabs,
        }
    }
}

/// `SelectTab`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectTab {
    Previous,
    Next,
    Last,
    N(usize),
}

/// The window as seen by the windowing protocol implementations (Zig
/// passes `*Window`).
struct WindowRef(glib::WeakRef<Window>);

impl winproto::ApprtWindow for WindowRef {
    fn window(&self) -> gtk4::Window {
        self.0.upgrade().expect("window outlives its winproto").upcast()
    }

    fn get_window_decoration(&self) -> WindowDecoration {
        self.0.upgrade().map(|w| w.get_window_decoration()).unwrap_or(WindowDecoration::Auto)
    }

    fn get_config(&self) -> Option<Rc<CoreConfig>> {
        let w = self.0.upgrade()?;
        let c = w.get_config()?;
        let snapshot = c.get().clone();
        Some(Rc::new(snapshot))
    }

    fn is_quick_terminal(&self) -> bool {
        self.0.upgrade().is_some_and(|w| w.is_quick_terminal())
    }
}

mod imp {
    use super::*;

    #[derive(gtk4::CompositeTemplate)]
    #[template(resource = "/com/mitchellh/ghostty/ui/1.4/window.ui")]
    pub struct Window {
        /// Whether this window is a quick terminal. If it is then it
        /// behaves slightly differently under certain scenarios.
        pub quick_terminal: Cell<bool>,

        /// Timeout source to react to this window becoming (in)active.
        pub handle_active_state_source: RefCell<Option<glib::SourceId>>,

        /// The window decoration override. If this is not set then we'll
        /// inherit whatever the config has. This allows overriding the
        /// config on a per-window basis.
        pub window_decoration: Cell<Option<WindowDecoration>>,

        /// Binding group for our active tab.
        pub tab_bindings: glib::BindingGroup,

        /// The configuration that this surface is using.
        pub config: RefCell<Option<Config>>,

        /// State and logic for windowing protocol for a window.
        pub winproto: RefCell<winproto::Window>,

        /// Kind of hacky to have this but this lets us know if we've
        /// initialized any single surface yet. We need this because we
        /// gate default size on this so that we don't resize the window
        /// after surfaces already exist.
        ///
        /// I think long term we can probably get rid of this by implementing
        /// a property or method that gets us all the surfaces in all the
        /// tabs and checking if we have zero or one that isn't initialized.
        ///
        /// For now, this logic is more similar to our legacy GTK side.
        pub surface_init: Cell<bool>,

        /// True if the running program requested a resize (see
        /// resizeSurface) that toplevelComputeSize should not override.
        pub resize_requested: Cell<bool>,

        /// See tabOverviewOpen for why we have this.
        pub tab_overview_focus_timer: RefCell<Option<glib::SourceId>>,

        /// A weak reference to a command palette.
        pub command_palette: glib::WeakRef<CommandPalette>,

        /// Tab page that the context menu was opened for.
        /// setup by `setup-menu`.
        pub context_menu_page: RefCell<Option<libadwaita::TabPage>>,

        /// The manually overridden title.
        pub title_override: RefCell<Option<String>>,

        /// Signal handlers on surfaces (Zig disconnects by user data).
        pub surface_handlers: RefCell<Vec<(glib::WeakRef<Surface>, glib::SignalHandlerId)>>,

        /// Signal handlers on tabs and their split trees.
        pub tab_handlers: RefCell<Vec<(glib::WeakRef<glib::Object>, glib::SignalHandlerId)>>,

        // Template bindings
        #[template_child]
        pub tab_overview: TemplateChild<libadwaita::TabOverview>,
        #[template_child]
        pub tab_bar: TemplateChild<libadwaita::TabBar>,
        #[template_child]
        pub tab_view: TemplateChild<libadwaita::TabView>,
        #[template_child]
        pub toolbar: TemplateChild<libadwaita::ToolbarView>,
        #[template_child]
        pub toast_overlay: TemplateChild<libadwaita::ToastOverlay>,
    }

    impl Default for Window {
        fn default() -> Self {
            Window {
                quick_terminal: Cell::new(false),
                handle_active_state_source: RefCell::new(None),
                window_decoration: Cell::new(None),
                tab_bindings: glib::BindingGroup::new(),
                config: RefCell::new(None),
                // We initialize our windowing protocol to none because we
                // can't actually initialize this until we get realized.
                winproto: RefCell::new(winproto::Window::None(winproto::noop::Window {})),
                surface_init: Cell::new(false),
                resize_requested: Cell::new(false),
                tab_overview_focus_timer: RefCell::new(None),
                command_palette: glib::WeakRef::new(),
                context_menu_page: RefCell::new(None),
                title_override: RefCell::new(None),
                surface_handlers: RefCell::new(Vec::new()),
                tab_handlers: RefCell::new(Vec::new()),
                tab_overview: Default::default(),
                tab_bar: Default::default(),
                tab_view: Default::default(),
                toolbar: Default::default(),
                toast_overlay: Default::default(),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Window {
        const NAME: &'static str = "GhosttyWindow";
        type Type = super::Window;
        type ParentType = libadwaita::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            DebugWarning::ensure_type();
            SplitTree::ensure_type();
            Surface::ensure_type();
            Tab::ensure_type();
            class::ensure_resources();
            klass.bind_template();
            super::bind_template_callbacks(klass);
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
            // SAFETY: the instance is initialized (as in `init_template`).
            unsafe { obj.as_ref() }.init();
        }
    }

    impl ObjectImpl for Window {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPS: LazyLock<Vec<glib::ParamSpec>> = LazyLock::new(|| {
                vec![
                    glib::ParamSpecObject::builder::<Surface>(properties::ACTIVE_SURFACE)
                        .read_only()
                        .build(),
                    glib::ParamSpecObject::builder::<Config>(properties::CONFIG).build(),
                    glib::ParamSpecBoolean::builder(properties::DEBUG)
                        .default_value(build_config::IS_DEBUG)
                        .read_only()
                        .build(),
                    glib::ParamSpecBoolean::builder(properties::HEADERBAR_VISIBLE)
                        .default_value(true)
                        .read_only()
                        .build(),
                    glib::ParamSpecBoolean::builder(properties::QUICK_TERMINAL)
                        .default_value(true)
                        .build(),
                    glib::ParamSpecBoolean::builder(properties::TABS_AUTOHIDE)
                        .default_value(true)
                        .read_only()
                        .build(),
                    glib::ParamSpecBoolean::builder(properties::TABS_INVERTED).read_only().build(),
                    glib::ParamSpecBoolean::builder(properties::TABS_VISIBLE)
                        .default_value(true)
                        .read_only()
                        .build(),
                    glib::ParamSpecBoolean::builder(properties::TABS_WIDE)
                        .default_value(true)
                        .read_only()
                        .build(),
                    glib::ParamSpecEnum::builder_with_default(
                        properties::TOOLBAR_STYLE,
                        libadwaita::ToolbarStyle::Raised,
                    )
                    .read_only()
                    .build(),
                    glib::ParamSpecEnum::builder_with_default(properties::TITLEBAR_STYLE, TitlebarStyle::Native)
                        .read_only()
                        .build(),
                    glib::ParamSpecString::builder(properties::TITLE_OVERRIDE).build(),
                ]
            });
            PROPS.as_ref()
        }

        fn property(&self, _: usize, pspec: &glib::ParamSpec) -> glib::Value {
            let obj = self.obj();
            match pspec.name() {
                properties::ACTIVE_SURFACE => obj.get_active_surface().to_value(),
                properties::CONFIG => accessor::get(&self.config),
                properties::DEBUG => build_config::IS_DEBUG.to_value(),
                properties::HEADERBAR_VISIBLE => obj.get_headerbar_visible().to_value(),
                properties::QUICK_TERMINAL => accessor::get_shallow(&self.quick_terminal),
                properties::TABS_AUTOHIDE => obj.get_tabs_autohide().to_value(),
                properties::TABS_INVERTED => obj.get_tabs_inverted().to_value(),
                properties::TABS_VISIBLE => obj.get_tabs_visible().to_value(),
                properties::TABS_WIDE => obj.get_tabs_wide().to_value(),
                properties::TOOLBAR_STYLE => obj.get_toolbar_style().to_value(),
                properties::TITLEBAR_STYLE => obj.get_titlebar_style().to_value(),
                properties::TITLE_OVERRIDE => accessor::get(&self.title_override),
                _ => unreachable!(),
            }
        }

        fn set_property(&self, _: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            match pspec.name() {
                properties::CONFIG => accessor::set(&self.config, value),
                properties::QUICK_TERMINAL => accessor::set_shallow(&self.quick_terminal, value),
                properties::TITLE_OVERRIDE => accessor::set(&self.title_override, value),
                _ => unreachable!(),
            }
        }

        fn dispose(&self) {
            if let Some(v) = self.handle_active_state_source.take() {
                v.remove();
            }

            self.command_palette.set(None);

            self.config.replace(None);

            self.tab_bindings.set_source(None::<&glib::Object>);

            self.dispose_template();
        }
    }

    impl Drop for Window {
        // Zig: `finalize`.
        fn drop(&mut self) {
            self.winproto.get_mut().deinit();
        }
    }

    impl WidgetImpl for Window {}
    impl WindowImpl for Window {}
    impl ApplicationWindowImpl for Window {}
    impl AdwApplicationWindowImpl for Window {}
}

glib::wrapper! {
    pub struct Window(ObjectSubclass<imp::Window>)
        @extends libadwaita::ApplicationWindow, gtk4::ApplicationWindow, gtk4::Window, gtk4::Widget,
        @implements gio::ActionGroup, gio::ActionMap, gtk4::Accessible, gtk4::Buildable,
            gtk4::ConstraintTarget, gtk4::Native, gtk4::Root, gtk4::ShortcutManager;
}

impl Window {
    pub fn new(app: &Application, title: Option<&str>) -> Window {
        let win: Window = glib::Object::builder().property("application", app).build();

        if let Some(title) = title {
            // If the overrides have a title set, we set that immediately
            // so that any applications inspecting the window states see an
            // immediate title set when the window appears, rather than
            // waiting possibly a few event loop ticks for it to sync from
            // the surface.
            win.set_title(Some(title));
        }

        win
    }

    fn init(&self) {
        // If our configuration is null then we get the configuration
        // from the application.
        let priv_ = self.imp();

        let config_obj = match self.get_config() {
            Some(c) => c,
            None => {
                let app = Application::default();
                let c = app.get_config();
                priv_.config.replace(c.clone());
                match c {
                    Some(c) => c,
                    None => return,
                }
            }
        };

        // Add our dev CSS class if we're in debug mode.
        if build_config::IS_DEBUG {
            self.add_css_class("devel");
        }

        // Setup our tab binding group. This ensures certain properties
        // are only synced from the currently active tab.
        priv_.tab_bindings.bind("title", self, "title").build();

        // Set our window icon. We can't set this in the blueprint file
        // because its dependent on the build config.
        self.set_icon_name(Some(build_config::BUNDLE_ID));

        // Initialize our actions
        self.init_action_map();

        // Start states based on config.
        {
            let config = config_obj.get();
            if config.maximize {
                self.maximize();
            }
            if config.fullscreen != crate::config::config::Fullscreen::False {
                self.fullscreen();
            }

            // If we have an explicit title set, we set that immediately
            // so that any applications inspecting the window states see
            // an immediate title set when the window appears, rather than
            // waiting possibly a few event loop ticks for it to sync from
            // the surface.
            if let Some(title) = &config.title {
                self.set_title(Some(title));
            }
        }

        // We always sync our appearance at the end because loading our
        // config and such can affect our bindings which are setup initially
        // in initTemplate.
        self.sync_appearance();

        // We need to do this so that the title initializes properly,
        // I think because its a dynamic getter.
        self.notify(properties::ACTIVE_SURFACE);
    }

    /// Setup our action map.
    fn init_action_map(&self) {
        let s = Some(glib::VariantTy::STRING);
        let actions = [
            GtkAction::init("about", Self::action_about, None),
            GtkAction::init("close", |_, _, w: &Window| w.close(), None),
            GtkAction::init("close-tab", Self::action_close_tab, s),
            GtkAction::init("new-tab", |_, _, w: &Window| w.perform_binding_action(BindingAction::NewTab), None),
            GtkAction::init(
                "new-window",
                |_, _, w: &Window| w.perform_binding_action(BindingAction::NewWindow),
                None,
            ),
            GtkAction::init(
                "prompt-surface-title",
                |_, _, w: &Window| w.perform_binding_action(BindingAction::PromptSurfaceTitle),
                None,
            ),
            GtkAction::init(
                "prompt-tab-title",
                |_, _, w: &Window| w.perform_binding_action(BindingAction::PromptTabTitle),
                None,
            ),
            GtkAction::init("prompt-context-tab-title", Self::action_prompt_context_tab_title, None),
            GtkAction::init(
                "prompt-window-title",
                |_, _, w: &Window| w.perform_binding_action(BindingAction::PromptWindowTitle),
                None,
            ),
            GtkAction::init("ring-bell", Self::action_ring_bell, None),
            GtkAction::init(
                "split-right",
                |_, _, w: &Window| w.perform_binding_action(BindingAction::NewSplit(SplitDirection::Right)),
                None,
            ),
            GtkAction::init(
                "split-left",
                |_, _, w: &Window| w.perform_binding_action(BindingAction::NewSplit(SplitDirection::Left)),
                None,
            ),
            GtkAction::init(
                "split-up",
                |_, _, w: &Window| w.perform_binding_action(BindingAction::NewSplit(SplitDirection::Up)),
                None,
            ),
            GtkAction::init(
                "split-down",
                |_, _, w: &Window| w.perform_binding_action(BindingAction::NewSplit(SplitDirection::Down)),
                None,
            ),
            GtkAction::init(
                "copy",
                |_, _, w: &Window| w.perform_binding_action(BindingAction::CopyToClipboard(CopyToClipboard::Mixed)),
                None,
            ),
            GtkAction::init(
                "paste",
                |_, _, w: &Window| w.perform_binding_action(BindingAction::PasteFromClipboard),
                None,
            ),
            GtkAction::init("reset", |_, _, w: &Window| w.perform_binding_action(BindingAction::Reset), None),
            GtkAction::init(
                "clear",
                |_, _, w: &Window| w.perform_binding_action(BindingAction::ClearScreen),
                None,
            ),
            // TODO: accept the surface that toggled the command palette
            GtkAction::init(
                "toggle-command-palette",
                |_, _, w: &Window| w.toggle_command_palette(),
                None,
            ),
            GtkAction::init("toggle-inspector", |_, _, w: &Window| w.toggle_inspector(), None),
        ];

        ext::actions::add(self, &actions);
    }

    /// Winproto backend for this window.
    pub fn with_winproto<R>(&self, f: impl FnOnce(&mut winproto::Window) -> R) -> R {
        f(&mut self.imp().winproto.borrow_mut())
    }

    /// Create a new tab with the given parent. The tab will be inserted
    /// at the position dictated by the `window-new-tab-position` config.
    /// The new tab will be selected.
    pub fn new_tab(&self, parent: Option<*mut CoreSurface>, overrides: &Overrides) {
        let _ = self.new_tab_page(parent, NewSurfaceContext::Tab, overrides);
    }

    pub fn new_tab_for_window(&self, parent: Option<*mut CoreSurface>, overrides: &Overrides) {
        let _ = self.new_tab_page(parent, NewSurfaceContext::Window, overrides);
    }

    fn new_tab_page(
        &self,
        parent: Option<*mut CoreSurface>,
        context: NewSurfaceContext,
        overrides: &Overrides,
    ) -> libadwaita::TabPage {
        let priv_ = self.imp();
        let tab_view = &*priv_.tab_view;

        // Create our new tab object
        let config = self.get_config();
        let tab = Tab::new(config.as_ref(), overrides);

        if let Some(p) = parent {
            // For a new window's first tab, inherit the parent's initial
            // size hints.
            if context == NewSurfaceContext::Window {
                // SAFETY: the parent is a live GTK core surface.
                let gobj = unsafe { crate::apprt::gtk::gobj_of(p) };
                self.surface_init(&gobj);
            }
            // SAFETY: the parent is live.
            tab.set_parent_with_context(unsafe { &*p }, context);
        }

        // Get the position that we should insert the new tab at.
        let Some(config) = config else {
            // If we don't have a config we just append it at the end.
            // This should never happen.
            return tab_view.append(&tab);
        };
        let new_tab_position = config.get().window_new_tab_position;
        let position = match new_tab_position {
            WindowNewTabPosition::Current => match tab_view.selected_page() {
                Some(selected) => tab_view.page_position(&selected) + 1,
                None => tab_view.n_pages(),
            },
            WindowNewTabPosition::End => tab_view.n_pages(),
        };

        // Add the page and select it
        let page = tab_view.insert(&tab, position);
        tab_view.set_selected_page(&page);

        // Create some property bindings
        tab.bind_property("title", &page, "title").sync_create().build();
        tab.bind_property("tooltip", &page, "tooltip").sync_create().build();

        page
    }

    /// Select the tab as requested. Returns true if the tab selection
    /// changed.
    pub fn select_tab(&self, n: SelectTab) -> bool {
        let tab_view = &*self.imp().tab_view;

        // Get our current tab numeric position
        let Some(selected) = tab_view.selected_page() else { return false };
        let current = tab_view.page_position(&selected);

        // Get our total
        let total = tab_view.n_pages();

        let goto = match n {
            SelectTab::Previous => {
                if current > 0 {
                    current - 1
                } else {
                    total - 1
                }
            }
            SelectTab::Next => {
                if current < total - 1 {
                    current + 1
                } else {
                    0
                }
            }
            SelectTab::Last => total - 1,
            SelectTab::N(v) => {
                // 1-indexed
                if v == 0 {
                    return false;
                }
                let Ok(n_int) = i32::try_from(v) else { return false };
                (n_int - 1).min(total - 1)
            }
        };
        debug_assert!(goto >= 0);
        debug_assert!(goto < total);

        // If our target is the same as our current then we do nothing.
        if goto == current {
            return false;
        }

        // Add the page and select it
        let page = tab_view.nth_page(goto);
        tab_view.set_selected_page(&page);

        true
    }

    /// Move the tab containing the given surface by the given amount.
    /// Returns if this affected any tab positioning.
    pub fn move_tab(&self, surface: &Surface, amount: isize) -> bool {
        let tab_view = &*self.imp().tab_view;

        // If we have one tab we never move.
        let total = tab_view.n_pages();
        if total == 1 {
            return false;
        }

        // Get the tab that contains the given surface.
        let Some(tab) = ext::get_ancestor::<Tab>(surface) else { return false };

        // Get the page position that contains the tab.
        let page = tab_view.page(&tab);
        let pos = tab_view.page_position(&page);

        // Move it
        let desired_pos = move_tab_position(pos, amount, total);
        debug_assert!(desired_pos >= 0);
        debug_assert!(desired_pos < total);

        tab_view.reorder_page(&page, desired_pos)
    }

    pub fn move_tab_to_new_window(&self, surface: &Surface) -> bool {
        let tab_view = &*self.imp().tab_view;
        let Some(tab) = ext::get_ancestor::<Tab>(surface) else { return false };
        let page = tab_view.page(&tab);

        // Skip if this is the only tab in the existing window
        if tab_view.n_pages() <= 1 {
            return false;
        }

        let app = Application::default();
        let window = Window::new(&app, None);

        tab_view.transfer_page(&page, &window.imp().tab_view, 0);
        window.present();
        true
    }

    pub fn toggle_tab_overview(&self) {
        let tab_overview = &*self.imp().tab_overview;
        let is_open = tab_overview.is_open();
        tab_overview.set_open(!is_open);
    }

    /// Toggle the visible property.
    pub fn toggle_visibility(&self) {
        self.set_visible(!self.is_visible());
    }

    /// Updates various appearance properties. This should always be safe
    /// to call multiple times. This should be called whenever a change
    /// happens that might affect how the window appears (config change,
    /// fullscreen, etc.).
    fn sync_appearance(&self) {
        let priv_ = self.imp();

        // Toggle style classes based on whether we're using CSDs or SSDs.
        //
        // These classes are defined in the gtk.Window documentation:
        // https://docs.gtk.org/gtk4/class.Window.html#css-nodes.
        {
            // Reset all style classes first
            for class in ["ssd", "csd", "solid-csd", "no-border-radius"] {
                self.remove_css_class(class);
            }

            let csd_enabled = priv_.winproto.borrow().client_side_decoration_enabled();
            self.set_decorated(csd_enabled);

            if csd_enabled {
                let display = WidgetExt::display(self);

                // We do the exact same check GTK is doing internally and
                // toggle either the `csd` or `solid-csd` style, based on
                // whether the user's window manager is deemed
                // _non-compositing_.
                //
                // In practice this only impacts users of traditional X11
                // window managers (e.g. i3, dwm, awesomewm, etc.) and not
                // X11 desktop environments or Wayland compositors/DEs.
                if display.is_rgba() && display.is_composited() {
                    self.add_css_class("csd");
                } else {
                    self.add_css_class("solid-csd");
                }
            } else {
                self.add_css_class("ssd");
                // Fix any artifacting that may occur in window corners.
                self.add_css_class("no-border-radius");
            }
        }

        // Trigger all our dynamic properties that depend on the config.
        for key in [
            properties::HEADERBAR_VISIBLE,
            properties::TABS_AUTOHIDE,
            properties::TABS_INVERTED,
            properties::TABS_VISIBLE,
            properties::TABS_WIDE,
            properties::TOOLBAR_STYLE,
            properties::TITLEBAR_STYLE,
        ] {
            self.notify(key);
        }

        // Remainder uses the config
        let Some(config_obj) = self.get_config() else { return };
        let (opacity, location) = {
            let config = config_obj.get();
            (config.background_opacity, config.gtk_tabs_location)
        };

        // Only add a solid background if we're opaque.
        self.toggle_css_class("background", opacity >= 1.0);

        // Move the tab bar to the proper location.
        priv_.toolbar.remove(&*priv_.tab_bar);
        match location {
            GtkTabsLocation::Top => priv_.toolbar.add_top_bar(&*priv_.tab_bar),
            GtkTabsLocation::Bottom => priv_.toolbar.add_bottom_bar(&*priv_.tab_bar),
        }

        // Do our window-protocol specific appearance sync.
        if let Err(err) = priv_.winproto.borrow_mut().sync_appearance() {
            log::warn!(target: LOG, "failed to sync winproto appearance error={err:?}");
        }
    }

    /// Sync the state of any actions on this window.
    fn sync_actions(&self) {
        let has_selection = self
            .get_active_surface()
            .and_then(|s| s.core())
            // SAFETY: the core surface is live while its GTK surface is.
            .is_some_and(|core| unsafe { (*core).has_selection() });

        let Some(action) = self.lookup_action("copy") else { return };
        let Ok(action) = action.downcast::<gio::SimpleAction>() else { return };
        action.set_enabled(has_selection);
    }

    fn toggle_css_class(&self, class: &str, value: bool) {
        if value {
            self.add_css_class(class);
        } else {
            self.remove_css_class(class);
        }
    }

    /// Perform a binding action on the window's active surface.
    fn perform_binding_action(&self, action: BindingAction) {
        let Some(surface) = self.get_active_surface() else { return };
        let Some(core_surface) = surface.core() else { return };
        // SAFETY: the core surface is live while its GTK surface is.
        if let Err(err) = unsafe { (*core_surface).perform_binding_action(action) } {
            log::warn!(target: LOG, "error performing binding action error={err}");
        }
    }

    /// Queue a simple text-based toast. All text-based toasts share the
    /// same timeout for consistency.
    //
    // This is not `pub` because we should be using signals emitted by
    // other widgets to trigger our toasts. Other objects should not
    // trigger toasts directly.
    fn add_toast(&self, title: &str) {
        let toast = libadwaita::Toast::new(title);
        toast.set_timeout(3);
        self.imp().toast_overlay.add_toast(toast);
    }

    fn connect_surface_handlers(&self, tree: &Tree) {
        let priv_ = self.imp();
        for entry in tree.iterator() {
            let surface = entry.view;
            // Before adding any new signal handlers, disconnect any that we
            // may have added before. Otherwise we may get multiple handlers
            // for the same signal.
            self.disconnect_handlers_of(surface);

            let mut ids = Vec::new();
            let weak = self.downgrade();
            ids.push(surface.connect_local(surface_class::signals::PRESENT_REQUEST, false, move |args| {
                let surface: Surface = args[0].get().ok()?;
                if let Some(this) = weak.upgrade() {
                    this.surface_present_request(&surface);
                }
                None
            }));
            let weak = self.downgrade();
            ids.push(surface.connect_local(surface_class::signals::CLIPBOARD_WRITE, false, move |args| {
                let clipboard: ClipboardType = args[1].get().ok()?;
                let cleared: bool = args[3].get().ok()?;
                if let Some(this) = weak.upgrade() {
                    this.surface_clipboard_write(clipboard.into(), cleared);
                }
                None
            }));
            let weak = self.downgrade();
            ids.push(surface.connect_local(surface_class::signals::MENU, false, move |_| {
                if let Some(this) = weak.upgrade() {
                    this.sync_actions();
                }
                None
            }));
            let weak = self.downgrade();
            ids.push(surface.connect_local(surface_class::signals::TOGGLE_FULLSCREEN, false, move |_| {
                if let Some(this) = weak.upgrade() {
                    if this.is_fullscreen() {
                        this.unfullscreen();
                    } else {
                        this.fullscreen();
                    }
                    // We react to the changes in the propFullscreen callback
                }
                None
            }));
            let weak = self.downgrade();
            ids.push(surface.connect_local(surface_class::signals::TOGGLE_MAXIMIZE, false, move |_| {
                if let Some(this) = weak.upgrade() {
                    if this.is_maximized() {
                        this.unmaximize();
                    } else {
                        this.maximize();
                    }
                    // We react to the changes in the propMaximized callback
                }
                None
            }));

            // If we've never had a surface initialize yet, then we register
            // this signal. Its theoretically possible to launch multiple
            // surfaces before init so we could register this on multiple and
            // that is not a problem because we'll check the flag again in
            // each handler.
            if !priv_.surface_init.get() {
                let weak = self.downgrade();
                ids.push(surface.connect_local(surface_class::signals::INIT, false, move |args| {
                    let surface: Surface = args[0].get().ok()?;
                    if let Some(this) = weak.upgrade() {
                        this.surface_init(&surface);
                    }
                    None
                }));
            }

            let mut handlers = priv_.surface_handlers.borrow_mut();
            for id in ids {
                handlers.push((surface.downgrade(), id));
            }
        }
    }

    fn disconnect_handlers_of(&self, surface: &Surface) {
        let mut handlers = self.imp().surface_handlers.borrow_mut();
        let mut keep = Vec::with_capacity(handlers.len());
        for (weak, id) in handlers.drain(..) {
            match weak.upgrade() {
                Some(s) if &s == surface => s.disconnect(id),
                Some(_) => keep.push((weak, id)),
                None => {}
            }
        }
        *handlers = keep;
    }

    /// Disconnect all the surface handlers for the given tree. This should
    /// be called whenever a tree is no longer present in the window, e.g.
    /// when a tab is detached or the tree changes.
    fn disconnect_surface_handlers(&self, tree: &Tree) {
        for entry in tree.iterator() {
            self.disconnect_handlers_of(entry.view);
        }
    }

    /// Callback to handle this window becoming active or inactive.
    /// Triggered by propIsActive with a timeout to debounce temporary
    /// changes in active state.
    fn handle_active_state(&self) {
        self.imp().handle_active_state_source.replace(None);

        // Hide quick-terminal if set to autohide
        if self.is_quick_terminal() {
            if let Some(cfg) = self.get_config() {
                if cfg.get().quick_terminal_autohide && !self.is_active() && self.is_visible() {
                    self.toggle_visibility();
                }
            }
        }

        // Don't change urgency if we're not the active window.
        if !self.is_active() {
            return;
        }

        if let Err(err) = self.with_winproto(|wp| wp.set_urgent(false)) {
            log::warn!(target: LOG, "winproto failed to reset urgency={err:?}");
        }
    }

    //---------------------------------------------------------------
    // Properties

    /// Whether this terminal is a quick terminal or not.
    pub fn is_quick_terminal(&self) -> bool {
        self.imp().quick_terminal.get()
    }

    /// Get the currently active surface. See the "active-surface" property.
    pub fn get_active_surface(&self) -> Option<Surface> {
        let tab = self.get_selected_tab()?;
        tab.get_active_surface()
    }

    /// Returns the configuration for this window.
    pub fn get_config(&self) -> Option<Config> {
        self.imp().config.borrow().clone()
    }

    /// Get the tab view for this window.
    pub fn get_tab_view(&self) -> libadwaita::TabView {
        self.imp().tab_view.get()
    }

    /// Get the current window decoration value for this window.
    pub fn get_window_decoration(&self) -> WindowDecoration {
        let priv_ = self.imp();
        if let Some(v) = priv_.window_decoration.get() {
            return v;
        }
        if let Some(v) = self.get_config() {
            return v.get().window_decoration;
        }
        WindowDecoration::Auto
    }

    /// Resize the window so that the given surface is the given size, as
    /// requested by the running program. See `apprt.action.ResizeWindow`.
    /// This is only done if the surface is the only terminal in the window
    /// and the compositor doesn't own the window size.
    pub fn resize_surface(&self, surface: &Surface, size: ResizeWindow) -> bool {
        if self.is_quick_terminal() || self.is_maximized() || self.is_fullscreen() || self.is_tiled() {
            return false;
        }
        if self.get_tab_view().n_pages() != 1 {
            return false;
        }
        let Some(tree) = ext::get_ancestor::<SplitTree>(surface) else { return false };
        if tree.get_is_split() {
            return false;
        }

        // The requested size is in the surface's content scale, which also
        // includes the font DPI, so we compute the change in device pixels
        // and then convert it to the window's logical pixels. Resizing the
        // window by the change accounts for the headerbar and tab bar.
        let scale = surface.get_content_scale();
        let current = surface.get_size();
        let factor = self.scale_factor() as f32;
        let dw = resize_delta(size.width, scale.x, current.width, factor);
        let dh = resize_delta(size.height, scale.y, current.height, factor);

        self.imp().resize_requested.set(true);
        self.set_default_size(self.width() + dw, self.height() + dh);
        true
    }

    /// Toggle the window decorations for this window.
    pub fn toggle_window_decorations(&self) {
        let priv_ = self.imp();

        if priv_.window_decoration.get().is_some() {
            // Unset any previously set window decoration settings
            self.set_window_decoration(None);
            return;
        }

        let Some(config) = self.get_config() else { return };
        let current = config.get().window_decoration;
        self.set_window_decoration(Some(match current {
            // Use auto when the decoration is initially none
            WindowDecoration::None => WindowDecoration::Auto,

            // Anything non-none to none
            WindowDecoration::Auto | WindowDecoration::Client | WindowDecoration::Server => WindowDecoration::None,
        }));
    }

    /// Set the window decoration override for this window. If this is null,
    /// then we'll revert back to the configuration's default.
    fn set_window_decoration(&self, new: Option<WindowDecoration>) {
        self.imp().window_decoration.set(new);
        self.sync_appearance();
    }

    /// Get the currently selected tab as a Tab object.
    fn get_selected_tab(&self) -> Option<Tab> {
        let page = self.imp().tab_view.selected_page()?;
        let child = page.child();
        debug_assert!(child.is::<Tab>());
        child.downcast().ok()
    }

    /// Returns true if this window needs confirmation before quitting.
    pub fn get_needs_confirm_quit(&self) -> bool {
        let tab_view = &*self.imp().tab_view;
        let n = tab_view.n_pages();
        debug_assert!(n >= 0);

        for i in 0..n {
            let page = tab_view.nth_page(i);
            let child = page.child();
            let Ok(tab) = child.downcast::<Tab>() else {
                log::warn!(target: LOG, "unexpected non-Tab child in tab view");
                continue;
            };
            if tab.get_needs_confirm_quit() {
                return true;
            }
        }

        false
    }

    fn is_tiled(&self) -> bool {
        let Some(surface) = self.surface() else { return false };
        let Some(toplevel) = surface.downcast_ref::<gdk::Toplevel>() else { return false };
        toplevel.state().contains(gdk::ToplevelState::TILED)
    }

    fn get_headerbar_visible(&self) -> bool {
        let priv_ = self.imp();

        // Never display the header bar when CSDs are disabled.
        let csd_enabled = priv_.winproto.borrow().client_side_decoration_enabled();
        if !csd_enabled {
            return false;
        }

        // Never display the header bar as a quick terminal.
        if priv_.quick_terminal.get() {
            return false;
        }

        // If we're fullscreen we never show the header bar.
        if self.is_fullscreen() {
            return false;
        }

        // The remainder needs a config
        let Some(config_obj) = self.get_config() else { return true };
        let config = config_obj.get();

        // *Conditionally* disable the header bar when maximized, and
        // gtk-titlebar-hide-when-maximized is set
        if self.is_maximized() && config.gtk_titlebar_hide_when_maximized {
            return false;
        }

        match config.gtk_titlebar_style {
            // If the titlebar style is tabs never show the titlebar.
            GtkTitlebarStyle::Tabs => false,

            // If the titlebar style is native show the titlebar if
            // configured to do so.
            GtkTitlebarStyle::Native => config.gtk_titlebar,
        }
    }

    fn get_tabs_autohide(&self) -> bool {
        let Some(config) = self.get_config() else { return true };
        let config = config.get();

        match config.gtk_titlebar_style {
            // If the titlebar style is tabs we cannot autohide.
            GtkTitlebarStyle::Tabs => false,

            GtkTitlebarStyle::Native => match config.window_show_tab_bar {
                // Auto we always autohide... obviously.
                WindowShowTabBar::Auto => true,

                // Always we never autohide because we always show the tab
                // bar.
                WindowShowTabBar::Always => false,

                // Never we autohide because it doesn't actually matter,
                // since getTabsVisible will return false.
                WindowShowTabBar::Never => true,
            },
        }
    }

    fn get_tabs_visible(&self) -> bool {
        let Some(config) = self.get_config() else { return true };
        let config = config.get();

        match config.gtk_titlebar_style {
            GtkTitlebarStyle::Tabs => {
                // *Conditionally* disable the tab bar when maximized, the
                // titlebar style is tabs, and gtk-titlebar-hide-when-maximized
                // is set.
                if self.is_maximized() && config.gtk_titlebar_hide_when_maximized {
                    return false;
                }

                // If the titlebar style is tabs the tab bar must always be
                // visible.
                true
            }
            GtkTitlebarStyle::Native => match config.window_show_tab_bar {
                WindowShowTabBar::Always | WindowShowTabBar::Auto => true,
                WindowShowTabBar::Never => false,
            },
        }
    }

    fn get_tabs_inverted(&self) -> bool {
        self.get_config().map(|c| c.get().gtk_tabs_inverted).unwrap_or(false)
    }

    fn get_tabs_wide(&self) -> bool {
        self.get_config().map(|c| c.get().gtk_wide_tabs).unwrap_or(true)
    }

    fn get_toolbar_style(&self) -> libadwaita::ToolbarStyle {
        let Some(config) = self.get_config() else { return libadwaita::ToolbarStyle::Raised };
        let style = config.get().gtk_toolbar_style;
        match style {
            GtkToolbarStyle::Flat => libadwaita::ToolbarStyle::Flat,
            GtkToolbarStyle::Raised => libadwaita::ToolbarStyle::Raised,
            GtkToolbarStyle::RaisedBorder => libadwaita::ToolbarStyle::RaisedBorder,
        }
    }

    fn get_titlebar_style(&self) -> TitlebarStyle {
        let Some(config) = self.get_config() else { return TitlebarStyle::Native };
        let style = config.get().gtk_titlebar_style;
        style.into()
    }

    fn prop_config(&self) {
        if let Some(config_obj) = self.get_config() {
            let reload = config_obj.get().app_notifications.config_reload;
            if reload {
                self.add_toast(class::i18n_(c"Reloaded the configuration"));
            }
        }

        self.sync_appearance();
    }

    fn prop_is_active(&self) {
        let priv_ = self.imp();

        // Use a timeout callback to wait for focus state to settle,
        // because depending on the windowing backend the window might
        // become inactive and immediately active again. This happens
        // e.g. on Wayland when opening a context menu or a submenu
        // inside a context menu.
        if priv_.handle_active_state_source.borrow().is_none() {
            let weak = self.downgrade();
            let id = glib::timeout_add_local_full(
                // 50ms was chosen to be conservative. From testing we know
                // that, depending on the backend and system performance, a
                // shorter timeout or just an idle callback can be enough for
                // the focus to settle. On the other hand a delay of e.g. 10ms
                // does not work reliably on some slow systems. The downside
                // of a high value is that some operations in
                // handleActiveState, e.g. hiding the quick-terminal, will be
                // visibly delayed. However, 50ms should barely be noticeable.
                // We can change this in the future if necessary.
                std::time::Duration::from_millis(50),
                // Use priority of an idle callback instead of the higher
                // default timeout priority. This allows us to use a shorter
                // timeout duration.
                glib::Priority::DEFAULT_IDLE,
                move || {
                    if let Some(this) = weak.upgrade() {
                        this.handle_active_state();
                    }
                    glib::ControlFlow::Break
                },
            );
            priv_.handle_active_state_source.replace(Some(id));
        }
    }

    fn prop_gdk_surface_dims(&self) {
        // X11 needs to fix blurring on resize, but winproto implementations
        // could do anything.
        if let Err(err) = self.with_winproto(|wp| wp.resize_event()) {
            log::warn!(target: LOG, "winproto resize event failed error={err:?}");
        }
    }

    fn toplevel_compute_size(&self, size: &mut gdk::ToplevelSize) {
        // The compositor/quick terminal own the size in these states.
        if self.is_maximized() || self.is_fullscreen() || self.is_quick_terminal() {
            return;
        }

        // Let GTK apply the default size set by a requested resize.
        let priv_ = self.imp();
        if priv_.resize_requested.get() {
            priv_.resize_requested.set(false);
            return;
        }

        // If there's no GdkSurface yet these dimensions will be zero size
        // which will make the window start out as small as possible. These
        // checks ensure we don't start with a 0, 0 window size.
        let Some(gdk_surface) = self.surface() else { return };
        let w = gdk_surface.width();
        let h = gdk_surface.height();
        if w <= 0 || h <= 0 {
            return;
        }

        // GTK clamps the requested size to the compositor-reported bounds,
        // which go stale when the window moves to a larger monitor. Only
        // re-assert the current size when it exceeds those bounds;
        // otherwise let GTK's default sizing win so a freshly-mapped window
        // isn't forced to a tiny size.
        let (bounds_w, bounds_h) = size.bounds();

        if bounds_w <= 0 || bounds_h <= 0 {
            return;
        }
        if w <= bounds_w && h <= bounds_h {
            return;
        }

        size.set_size(w, h);
    }

    fn prop_quick_terminal(&self) {
        let priv_ = self.imp();
        if priv_.surface_init.get() {
            log::warn!(target: LOG, "quick terminal property can't be changed after surfaces have been initialized");
            return;
        }

        if priv_.quick_terminal.get() {
            // Initialize the quick terminal at the app-layer
            let wref = WindowRef(self.downgrade());
            let result = Application::default().with_winproto(|wp| wp.init_quick_terminal(&wref));
            if let Some(Err(err)) = result {
                log::warn!(target: LOG, "failed to initialize quick terminal error={err:?}");
            }
        }
    }

    fn prop_scale_factor(&self) {
        // On some platforms (namely X11) we need to refresh our appearance
        // when the scale factor changes. In theory this could be more
        // fine-grained as a full refresh could be expensive, but a) this
        // *should* be rare, and b) quite noticeable visual bugs would occur
        // if this is not present.
        if let Err(err) = self.with_winproto(|wp| wp.sync_appearance()) {
            log::warn!(target: LOG, "failed to sync appearance after scale factor has been updated={err:?}");
        }
    }

    pub fn set_title_override(&self, title: Option<&str>) {
        self.imp().title_override.replace(title.map(str::to_owned));
        self.notify(properties::TITLE_OVERRIDE);
    }

    pub fn prompt_window_title(&self) {
        let title = self.title().map(|t| t.to_string());
        let initial = self.imp().title_override.borrow().clone().or(title);

        let dialog = TitleDialog::new(title_dialog::Target::Window, initial.as_deref());
        let weak = self.downgrade();
        dialog.connect_set(move |_, title| {
            if let Some(this) = weak.upgrade() {
                this.set_title_override(if title.is_empty() { None } else { Some(title) });
            }
        });

        dialog.present(self.upcast_ref());
    }

    //---------------------------------------------------------------
    // Signal handlers

    fn window_realize(&self) {
        let app = Application::default();

        // Initialize our window protocol logic
        let apprt_window: Rc<dyn winproto::ApprtWindow> = Rc::new(WindowRef(self.downgrade()));
        match app.with_winproto(|wp| winproto::Window::init(wp, apprt_window)) {
            Some(Ok(wp)) => {
                self.imp().winproto.replace(wp);
            }
            Some(Err(err)) => {
                log::warn!(target: LOG, "failed to initialize window protocol error={err:?}");
                return;
            }
            None => return,
        }

        // We need to setup resize notifications on our surface,
        // which is only available after the window had been realized.
        if let Some(gdk_surface) = self.surface() {
            let weak = self.downgrade();
            gdk_surface.connect_notify_local(Some("width"), move |_, _| {
                if let Some(this) = weak.upgrade() {
                    this.prop_gdk_surface_dims();
                }
            });
            let weak = self.downgrade();
            gdk_surface.connect_notify_local(Some("height"), move |_, _| {
                if let Some(this) = weak.upgrade() {
                    this.prop_gdk_surface_dims();
                }
            });
            // Connect after GTK's compute-size handler so our size wins.
            if let Some(toplevel) = gdk_surface.downcast_ref::<gdk::Toplevel>() {
                connect_compute_size_after(toplevel, self);
            }
        }

        // Notify every displayed surface when the compositor changes the
        // Wayland xdg_toplevel suspended state.
        let weak = self.downgrade();
        self.connect_notify_local(Some("suspended"), move |_, _| {
            if let Some(this) = weak.upgrade() {
                this.prop_suspended();
            }
        });

        // When we are realized we always setup our appearance since this
        // calls some winproto functions.
        self.sync_appearance();
    }

    fn prop_suspended(&self) {
        let Some(tab) = self.get_selected_tab() else { return };
        let Some(tree) = tab.get_surface_tree() else { return };

        for entry in tree.iterator() {
            entry.view.update_occlusion();
        }
    }

    fn tab_overview_create_tab(&self) -> libadwaita::TabPage {
        let parent = self.get_active_surface().and_then(|v| v.core());
        self.new_tab_page(parent, NewSurfaceContext::Tab, &Overrides::NONE)
    }

    fn tab_overview_open(&self, tab_overview: &libadwaita::TabOverview) {
        // We only care about when the tab overview is closed.
        if tab_overview.is_open() {
            return;
        }

        // On tab overview close, focus is sometimes lost. This is an
        // upstream issue in libadwaita[1]. When this is resolved we
        // can put a runtime version check here to avoid this workaround.
        //
        // Our workaround is to start a timer after 500ms to refocus
        // the currently selected tab. We choose 500ms because the adw
        // animation is 400ms.
        //
        // [1]: https://gitlab.gnome.org/GNOME/libadwaita/-/issues/670

        // If we have an old timer remove it
        let priv_ = self.imp();
        if let Some(timer) = priv_.tab_overview_focus_timer.take() {
            timer.remove();
        }

        // Restart our timer
        let weak = self.downgrade();
        let id = glib::timeout_add_local_once(std::time::Duration::from_millis(500), move || {
            let Some(this) = weak.upgrade() else { return };

            // Always note our timer is removed
            this.imp().tab_overview_focus_timer.replace(None);

            // Get our currently active surface which should respect the
            // newly selected tab. Grab focus.
            if let Some(surface) = this.get_active_surface() {
                surface.grab_focus();
            }
        });
        priv_.tab_overview_focus_timer.replace(Some(id));
    }

    fn window_close_request(&self) -> bool {
        if self.get_needs_confirm_quit() {
            // Show a confirmation dialog
            let dialog = CloseConfirmationDialog::new(close_confirmation_dialog::Target::Window);
            let weak = self.downgrade();
            dialog.connect_close_request(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.destroy();
                }
            });

            // Show it
            dialog.present(Some(self.upcast_ref()));
            return true;
        }

        self.destroy();
        false
    }

    fn tab_view_close_page(&self, page: &libadwaita::TabPage) -> bool {
        let tab_view = self.imp().tab_view.get();
        let child = page.child();
        let Ok(tab) = child.clone().downcast::<Tab>() else { return false };

        // If the tab says it doesn't need confirmation then we go ahead
        // and close immediately.
        if !tab.get_needs_confirm_quit() {
            tab_view.close_page_finish(page, true);
            return true;
        }

        // Show a confirmation dialog
        let dialog = CloseConfirmationDialog::new(close_confirmation_dialog::Target::Tab);
        let p = page.clone();
        dialog.connect_close_request(move |_| close_confirmation_finish_tab(&p, true));
        let p = page.clone();
        dialog.connect_cancel(move |_| close_confirmation_finish_tab(&p, false));

        // Show it
        dialog.present(Some(&child));
        true
    }

    fn tab_view_selected_page(&self) {
        let priv_ = self.imp();

        // Always reset our binding source in case we have no pages.
        priv_.tab_bindings.set_source(None::<&glib::Object>);

        // Get our current page which MUST be a Tab object.
        let Some(page) = priv_.tab_view.selected_page() else { return };
        let child = page.child();
        debug_assert!(child.is::<Tab>());

        // Setup our binding group. This ensures things like the title
        // are synced from the active tab.
        priv_.tab_bindings.set_source(Some(&child));

        // If the tab was previously marked as needing attention
        // (e.g. due to a bell character), we now unmark that
        page.set_needs_attention(false);
    }

    fn tab_view_page_attached(&self, page: &libadwaita::TabPage) {
        // Get the attached page which must be a Tab object.
        let child = page.child();
        let Ok(tab) = child.downcast::<Tab>() else { return };
        let priv_ = self.imp();

        // Attach listeners for the tab.
        let weak = self.downgrade();
        let id = tab.connect_close_request(move |tab| {
            if let Some(this) = weak.upgrade() {
                this.tab_close_request(tab);
            }
        });
        priv_
            .tab_handlers
            .borrow_mut()
            .push((tab.upcast_ref::<glib::Object>().downgrade(), id));

        // Attach listeners for the surface.
        //
        // Interesting behavior here that was previously undocumented but
        // I'm going to make it explicit here: we accept all the signals here
        // (like toggle-fullscreen) regardless of whether the surface or tab
        // is focused. At the time of writing this we have no API that could
        // really trigger these that way but its theoretically possible.
        //
        // What is DEFINITELY possible is something like OSC52 triggering
        // a clipboard-write signal on an unfocused tab/surface. We
        // definitely want to show the user a notification about that but our
        // notification right now is a toast that doesn't make it clear WHO
        // used the clipboard. We probably want to change that in the future.
        //
        // I'm not sure how desirable all the above is, and we probably
        // should be thoughtful about future signals here. But all of this
        // behavior is consistent with macOS and the previous GTK apprt,
        // but that behavior was all implicit and not documented, so here
        // I am.
        if let Some(tree) = tab.get_surface_tree() {
            self.connect_surface_handlers(&tree);
        }

        // Keep our surface handlers in sync as the tree changes. This is
        // connected here rather than when the tab is created so that tabs
        // moved between windows notify the window that they are in.
        let split_tree = tab.get_split_tree();
        let weak = self.downgrade();
        let id = split_tree.connect_local(crate::apprt::gtk::class::split_tree::signals::CHANGED, false, move |args| {
            let old: Option<Tree> = args[1].get().ok()?;
            let new: Option<Tree> = args[2].get().ok()?;
            if let Some(this) = weak.upgrade() {
                this.tab_split_tree_changed(old.as_ref(), new.as_ref());
            }
            None
        });
        priv_
            .tab_handlers
            .borrow_mut()
            .push((split_tree.upcast_ref::<glib::Object>().downgrade(), id));
    }

    fn tab_view_page_detached(&self, page: &libadwaita::TabPage) {
        // We need to get the tab to disconnect the signals.
        let child = page.child();
        let Ok(tab) = child.downcast::<Tab>() else { return };
        let split_tree = tab.get_split_tree();

        {
            let mut handlers = self.imp().tab_handlers.borrow_mut();
            let mut keep = Vec::with_capacity(handlers.len());
            for (weak, id) in handlers.drain(..) {
                match weak.upgrade() {
                    Some(o) if o == *tab.upcast_ref::<glib::Object>() || o == *split_tree.upcast_ref::<glib::Object>() => {
                        o.disconnect(id)
                    }
                    Some(_) => keep.push((weak, id)),
                    None => {}
                }
            }
            *handlers = keep;
        }

        // Remove the tree handlers
        if let Some(tree) = tab.get_surface_tree() {
            self.disconnect_surface_handlers(&tree);
        }
    }

    fn tab_view_create_window(&self) -> libadwaita::TabView {
        // Create a new window without creating a new tab.
        let win: Window = glib::Object::builder()
            .property("application", Application::default())
            .build();

        // We have to show it otherwise it'll just be hidden.
        win.present();

        // Get our tab view
        win.imp().tab_view.get()
    }

    fn tab_close_request(&self, tab: &Tab) {
        let tab_view = &*self.imp().tab_view;
        let page = tab_view.page(tab);
        // TODO: connect close page handler to tab to check for confirmation
        tab_view.close_page(&page);
    }

    fn tab_view_n_pages(&self) {
        let priv_ = self.imp();
        if priv_.tab_view.n_pages() == 0 {
            // If we have no pages left then we want to close window.

            // If the tab overview is open, then we don't close the window
            // because its a rather abrupt experience. This also fixes an
            // issue where dragging out the last tab in the tab overview
            // won't cause Ghostty to exit.
            if priv_.tab_overview.is_open() {
                return;
            }

            self.close();
        }
    }

    fn surface_clipboard_write(&self, clipboard_type: Clipboard, cleared: bool) {
        // We only toast for the standard clipboard.
        if clipboard_type != Clipboard::Standard {
            return;
        }

        // We only toast if configured to
        let Some(config_obj) = self.get_config() else { return };
        if !config_obj.get().app_notifications.clipboard_copy {
            return;
        }

        if !cleared {
            self.add_toast(class::i18n_(c"Copied to clipboard"));
        } else {
            self.add_toast(class::i18n_(c"Cleared clipboard"));
        }
    }

    fn surface_present_request(&self, surface: &Surface) {
        // Verify that this surface is actually in this window.
        {
            let Some(surface_window) = ext::get_ancestor::<Window>(surface) else {
                log::warn!(target: LOG, "present request called for non-existent surface");
                return;
            };
            if &surface_window != self {
                log::warn!(target: LOG, "present request called for surface in different window");
                return;
            }
        }

        // Get the tab for this surface.
        let Some(tab) = ext::get_ancestor::<Tab>(surface) else {
            log::warn!(target: LOG, "present request surface not found");
            return;
        };

        // Get the page that contains this tab
        let tab_view = &*self.imp().tab_view;
        let page = tab_view.page(&tab);
        tab_view.set_selected_page(&page);

        // Grab focus
        surface.grab_focus();

        // Bring the window to the front.
        self.present();
    }

    fn surface_init(&self, surface: &Surface) {
        let priv_ = self.imp();

        // Make sure we init only once
        if priv_.surface_init.get() {
            return;
        }
        priv_.surface_init.set(true);

        // Setup our default and minimum size.
        if let Some(size) = surface.get_default_size() {
            self.set_default_size(size.width as i32, size.height as i32);
        }
        if let Some(size) = surface.get_min_size() {
            self.set_size_request(size.width as i32, size.height as i32);
        }
    }

    fn tab_split_tree_changed(&self, old_tree: Option<&Tree>, new_tree: Option<&Tree>) {
        if let Some(tree) = old_tree {
            self.disconnect_surface_handlers(tree);
        }

        if let Some(tree) = new_tree {
            self.connect_surface_handlers(tree);
        }
    }

    fn action_about(_: &gio::SimpleAction, _: Option<&glib::Variant>, self_: &Self) {
        let name = "Ghostty";
        let icon = "com.mitchellh.ghostty";
        let website = "https://ghostty.org";

        let dialog = libadwaita::AboutDialog::builder()
            .application_name(name)
            .developer_name(class::i18n_(c"Ghostty Developers"))
            .application_icon(icon)
            .version(build_config::VERSION_STRING)
            .issue_url("https://github.com/ghostty-org/ghostty/issues")
            .website(website)
            .build();
        dialog.present(Some(self_));
    }

    fn action_close_tab(_: &gio::SimpleAction, param: Option<&glib::Variant>, self_: &Self) {
        let Some(param) = param else {
            log::warn!(target: LOG, "win.close-tab called without a parameter");
            return;
        };

        let Some(str) = param.str() else {
            log::warn!(target: LOG, "invalid mode provided to win.close-tab");
            return;
        };

        let mode = match str {
            "this" => CloseTabMode::This,
            "other" => CloseTabMode::Other,
            "right" => CloseTabMode::Right,
            _ => {
                log::warn!(target: LOG, "invalid mode provided to win.close-tab: {str}");
                return;
            }
        };

        self_.perform_binding_action(BindingAction::CloseTab(mode));
    }

    fn action_prompt_context_tab_title(_: &gio::SimpleAction, _: Option<&glib::Variant>, self_: &Self) {
        let Some(page) = self_.imp().context_menu_page.borrow().clone() else { return };
        let child = page.child();
        let Ok(tab) = child.downcast::<Tab>() else { return };
        tab.prompt_tab_title();
    }

    fn action_ring_bell(_: &gio::SimpleAction, _: Option<&glib::Variant>, self_: &Self) {
        let Some(config) = self_.get_config() else { return };
        let features = config.get().bell_features;

        if features.system {
            match self_.surface() {
                Some(native) => native.beep(),
                None => log::warn!(target: LOG, "unable to get native surface from window"),
            }
        }

        if features.attention {
            // Dont set urgency if the window is already active.
            if !self_.is_active() {
                // Request user attention
                if let Err(err) = self_.with_winproto(|wp| wp.set_urgent(true)) {
                    log::warn!(target: LOG, "winproto failed to set urgency={err:?}");
                }
            }
        }
    }

    /// Toggle the command palette.
    ///
    /// TODO: accept the surface that toggled the command palette as a
    /// parameter
    fn toggle_command_palette(&self) {
        let priv_ = self.imp();

        // Get a reference to a command palette. First check the weak
        // reference that we save to see if we already have one stored. If we
        // don't then create a new one.
        let command_palette = match priv_.command_palette.upgrade() {
            Some(c) => c,
            None => {
                // Create a fresh command palette.
                let command_palette = CommandPalette::new();

                // Synchronize our config to the command palette's config.
                self.bind_property("config", &command_palette, "config")
                    .sync_create()
                    .build();

                // Listen to the activate signal to know if the user selected
                // an option in the command palette.
                let weak = self.downgrade();
                command_palette.connect_trigger(move |_, action| {
                    // If the activation actually has an action, perform it.
                    if let Some(this) = weak.upgrade() {
                        this.perform_binding_action(action.clone());
                    }
                });

                // Save a weak reference to the command palette. We use a
                // weak reference to avoid reference counting cycles that
                // might cause problems later.
                priv_.command_palette.set(Some(&command_palette));

                command_palette
            }
        };

        // Tell the command palette to toggle itself. If the dialog gets
        // presented (instead of hidden) it will be modal over our window.
        command_palette.toggle(self);
    }

    /// Toggle the Ghostty inspector for the active surface.
    fn toggle_inspector(&self) {
        let Some(surface) = self.get_active_surface() else { return };
        surface.control_inspector(crate::apprt::action::Inspector::Toggle);
    }
}

fn close_confirmation_finish_tab(page: &libadwaita::TabPage, close: bool) {
    let Some(tab_view) = ext::get_ancestor::<libadwaita::TabView>(&page.child()) else {
        log::warn!(target: LOG, "close confirmation called for non-existent page");
        return;
    };
    tab_view.close_page_finish(page, close);
}

/// Returns the change in logical pixels from the current device pixel
/// size to the requested size, or zero if the requested size is zero.
fn resize_delta(requested: u32, scale: f32, current: u32, factor: f32) -> i32 {
    if requested == 0 {
        return 0;
    }
    let px = requested as f32 * scale;
    ((px - current as f32) / factor).round() as i32
}

/// The wrapped tab position for `moveTab`.
fn move_tab_position(pos: i32, amount: isize, total: i32) -> i32 {
    let initial = (pos as isize + amount) as i32;
    let max = total - 1;
    if initial < 0 {
        max + initial + 1
    } else if initial > max {
        initial - max - 1
    } else {
        initial
    }
}

fn closure_titlebar_style_is_tab(value: TitlebarStyle) -> bool {
    match value {
        TitlebarStyle::Native => false,
        TitlebarStyle::Tabs => true,
    }
}

fn closure_title(title: Option<&str>, title_override: Option<&str>) -> String {
    if let Some(v) = title_override {
        return v.to_owned();
    }
    if let Some(v) = title {
        return v.to_owned();
    }
    "Ghostty".to_owned()
}

fn closure_subtitle(config: Option<&Config>, pwd: Option<&str>) -> Option<String> {
    let config = config?;
    let subtitle = config.get().window_subtitle;
    match subtitle {
        WindowSubtitle::False => None,
        WindowSubtitle::WorkingDirectory => pwd.map(str::to_owned),
    }
}

/// Connect `compute-size` after GTK's own handler (gtk-rs only connects
/// before).
fn connect_compute_size_after(toplevel: &gdk::Toplevel, window: &Window) {
    use glib::translate::ToGlibPtr;
    unsafe extern "C" fn trampoline(
        _toplevel: *mut gdk::ffi::GdkToplevel,
        size: *mut gdk::ffi::GdkToplevelSize,
        data: glib::ffi::gpointer,
    ) {
        let weak = &*(data as *const glib::WeakRef<Window>);
        if let Some(window) = weak.upgrade() {
            // SAFETY: `ToplevelSize` is a transparent wrapper of the
            // (non-null) pointer GDK passes for the duration of the signal.
            let mut size: gdk::ToplevelSize =
                std::mem::transmute::<std::ptr::NonNull<gdk::ffi::GdkToplevelSize>, gdk::ToplevelSize>(
                    std::ptr::NonNull::new_unchecked(size),
                );
            window.toplevel_compute_size(&mut size);
            std::mem::forget(size);
        }
    }
    unsafe extern "C" fn destroy(data: glib::ffi::gpointer, _: *mut glib::gobject_ffi::GClosure) {
        drop(Box::from_raw(data as *mut glib::WeakRef<Window>));
    }

    let data = Box::into_raw(Box::new(window.downgrade()));
    // SAFETY: the trampoline matches the signal's C signature and the
    // boxed weak ref is freed by the destroy notify.
    unsafe {
        glib::gobject_ffi::g_signal_connect_data(
            ToGlibPtr::<*mut gdk::ffi::GdkToplevel>::to_glib_none(toplevel).0 as *mut glib::gobject_ffi::GObject,
            c"compute-size".as_ptr(),
            Some(std::mem::transmute::<*const (), unsafe extern "C" fn()>(trampoline as *const ())),
            data as glib::ffi::gpointer,
            Some(destroy),
            glib::gobject_ffi::G_CONNECT_AFTER,
        );
    }
}

fn bind_template_callbacks(klass: &mut <imp::Window as ObjectSubclass>::Class) {
    use class::bind_template_callback as cb;

    fn this(args: &class::TemplateArgs<'_>) -> Window {
        args.template()
    }

    cb(klass, "realize", |args| {
        this(args).window_realize();
        None
    });
    cb(klass, "new_tab", |args| {
        this(args).perform_binding_action(BindingAction::NewTab);
        None
    });
    cb(klass, "overview_create_tab", |args| Some(this(args).tab_overview_create_tab().to_value()));
    cb(klass, "overview_notify_open", |args| {
        let overview: libadwaita::TabOverview = args.get(0);
        this(args).tab_overview_open(&overview);
        None
    });
    cb(klass, "close_request", |args| Some(this(args).window_close_request().to_value()));
    cb(klass, "close_page", |args| {
        let page: libadwaita::TabPage = args.get(1);
        Some(this(args).tab_view_close_page(&page).to_value())
    });
    cb(klass, "page_attached", |args| {
        let page: libadwaita::TabPage = args.get(1);
        this(args).tab_view_page_attached(&page);
        None
    });
    cb(klass, "page_detached", |args| {
        let page: libadwaita::TabPage = args.get(1);
        this(args).tab_view_page_detached(&page);
        None
    });
    cb(klass, "setup_tab_menu", |args| {
        let page: Option<libadwaita::TabPage> = args.get(1);
        this(args).imp().context_menu_page.replace(page);
        None
    });
    cb(klass, "tab_create_window", |args| Some(this(args).tab_view_create_window().to_value()));
    cb(klass, "notify_n_pages", |args| {
        this(args).tab_view_n_pages();
        None
    });
    cb(klass, "notify_selected_page", |args| {
        this(args).tab_view_selected_page();
        None
    });
    cb(klass, "notify_config", |args| {
        this(args).prop_config();
        None
    });
    cb(klass, "notify_fullscreened", |args| {
        this(args).sync_appearance();
        None
    });
    cb(klass, "notify_is_active", |args| {
        this(args).prop_is_active();
        None
    });
    cb(klass, "notify_maximized", |args| {
        this(args).sync_appearance();
        None
    });
    cb(klass, "notify_menu_active", |args| {
        // We only care if we're activating. If we're activating then
        // we need to check the validity of our menu items.
        let button: gtk4::MenuButton = args.get(0);
        if button.is_active() {
            this(args).sync_actions();
        }
        None
    });
    cb(klass, "notify_quick_terminal", |args| {
        this(args).prop_quick_terminal();
        None
    });
    cb(klass, "notify_scale_factor", |args| {
        this(args).prop_scale_factor();
        None
    });
    cb(klass, "titlebar_style_is_tabs", |args| {
        let value: TitlebarStyle = args.get(1);
        Some(closure_titlebar_style_is_tab(value).to_value())
    });
    cb(klass, "computed_title", |args| {
        let title: Option<String> = args.get(2);
        let title_override: Option<String> = args.get(3);
        Some(closure_title(title.as_deref(), title_override.as_deref()).to_value())
    });
    cb(klass, "computed_subtitle", |args| {
        let config: Option<Config> = args.get(1);
        let pwd: Option<String> = args.get(2);
        Some(closure_subtitle(config.as_ref(), pwd.as_deref()).to_value())
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_delta_() {
        assert_eq!(resize_delta(0, 1.0, 100, 1.0), 0);
        assert_eq!(resize_delta(200, 1.0, 100, 1.0), 100);
        assert_eq!(resize_delta(200, 2.0, 100, 2.0), 150);
        assert_eq!(resize_delta(50, 1.0, 100, 1.0), -50);
    }

    #[test]
    fn move_tab_position_wraps() {
        assert_eq!(move_tab_position(0, 1, 3), 1);
        assert_eq!(move_tab_position(2, 1, 3), 0);
        assert_eq!(move_tab_position(0, -1, 3), 2);
        assert_eq!(move_tab_position(1, -1, 3), 0);
    }

    #[test]
    fn closures() {
        assert!(closure_titlebar_style_is_tab(TitlebarStyle::Tabs));
        assert!(!closure_titlebar_style_is_tab(TitlebarStyle::Native));
        assert_eq!(closure_title(None, None), "Ghostty");
        assert_eq!(closure_title(Some("a"), None), "a");
        assert_eq!(closure_title(Some("a"), Some("b")), "b");
        assert_eq!(BLUEPRINT.minor, 4);
    }
}
