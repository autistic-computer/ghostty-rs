//! Port of `src/apprt/gtk/class/command_palette.zig`.
//!
//! STATUS: ported.
//!
//! Zig's `new` sinks the palette and keeps a reference that is dropped
//! when the dialog closes; the port keeps that reference in a private
//! `self_ref` field. The command arena is replaced by owned strings.
#![cfg(feature = "gtk")]

use std::cell::RefCell;
use std::cmp::Ordering;
use std::sync::LazyLock;

use glib::prelude::*;
use glib::subclass::Signal;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use libadwaita::prelude::*;
use libadwaita::subclass::prelude::*;

use crate::apprt::gtk::class::{self, accessor, i18n_, Blueprint, Config};
use crate::apprt::gtk::key;
use crate::apprt::gtk::weak_ref::WeakRef;
use crate::input::binding::Action;
use crate::input::command::Command as InputCommand;

use crate::apprt::gtk::class::application::Application;
use crate::apprt::gtk::class::surface::Surface;
use crate::apprt::gtk::class::window::Window;

const LOG: &str = "gtk_ghostty_command_palette";

/// The template of this class (Zig: `gresource.blueprint(...)`).
pub const BLUEPRINT: Blueprint = Blueprint { major: 1, minor: 5, name: "command-palette" };

/// Property names (Zig: `properties`).
pub mod properties {
    pub const CONFIG: &str = "config";
}

/// Signal names (Zig: `signals`).
pub mod signals {
    /// Emitted when a command from the command palette is activated. The
    /// action contains pointers to allocated data so if a receiver of this
    /// signal needs to keep the action around it will need to clone the
    /// action or there may be use-after-free errors.
    ///
    /// The parameter is a `G_TYPE_POINTER` to an [`crate::input::binding::Action`]
    /// (see [`super::CommandPalette::connect_trigger`]).
    pub const TRIGGER: &str = "trigger";
}

mod imp {
    use super::*;

    #[derive(Default, gtk4::CompositeTemplate)]
    #[template(resource = "/com/mitchellh/ghostty/ui/1.5/command-palette.ui")]
    pub struct CommandPalette {
        /// The configuration that this command palette is using.
        pub config: RefCell<Option<Config>>,

        /// The reference the palette holds on itself until its dialog
        /// closes (Zig: `refSink` in `new`, `unref` in `dialogClosed`).
        pub self_ref: RefCell<Option<super::CommandPalette>>,

        /// The dialog object containing the palette UI.
        #[template_child]
        pub dialog: TemplateChild<libadwaita::Dialog>,

        /// The search input text field.
        #[template_child]
        pub search: TemplateChild<gtk4::SearchEntry>,

        /// The view containing each result row.
        #[template_child]
        pub view: TemplateChild<gtk4::ListView>,

        /// The model that provides filtered data for the view to display.
        #[template_child]
        pub model: TemplateChild<gtk4::SingleSelection>,

        /// The list that serves as the data source of the model.
        /// This is where all command data is ultimately stored.
        #[template_child]
        pub source: TemplateChild<gio::ListStore>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CommandPalette {
        const NAME: &'static str = "GhosttyCommandPalette";
        type Type = super::CommandPalette;
        type ParentType = libadwaita::Bin;

        fn class_init(klass: &mut Self::Class) {
            super::Command::ensure_type();
            class::ensure_resources();
            klass.bind_template();

            // Template Callbacks
            class::bind_template_callback(klass, "closed", |args| {
                let this: super::CommandPalette = args.template();
                this.dialog_closed();
                None
            });
            class::bind_template_callback(klass, "notify_config", |args| {
                let this: super::CommandPalette = args.template();
                this.prop_config();
                None
            });
            class::bind_template_callback(klass, "search_stopped", |args| {
                // ESC was pressed - close the palette
                let this: super::CommandPalette = args.template();
                this.close();
                None
            });
            class::bind_template_callback(klass, "search_activated", |args| {
                // If Enter is pressed, activate the selected entry
                let this: super::CommandPalette = args.template();
                let selected = this.imp().model.selected();
                this.activated(selected);
                None
            });
            class::bind_template_callback(klass, "row_activated", |args| {
                // (view, pos: c_uint, self)
                let this: super::CommandPalette = args.template();
                this.activated(args.get::<u32>(1));
                None
            });
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();

            // Listen for any changes to our config.
            // SAFETY: the instance is initialized (as in `init_template`).
            let this = unsafe { obj.as_ref() };
            this.connect_notify_local(Some(properties::CONFIG), |this, _| this.prop_config());
        }
    }

    impl ObjectImpl for CommandPalette {
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
                properties::CONFIG => accessor::set(&self.config, value),
                _ => unreachable!(),
            }
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: LazyLock<Vec<Signal>> = LazyLock::new(|| {
                vec![Signal::builder(signals::TRIGGER)
                    .param_types([glib::Type::POINTER])
                    .build()]
            });
            SIGNALS.as_ref()
        }

        fn dispose(&self) {
            self.source.remove_all();
            self.config.replace(None);
            self.dispose_template();
        }
    }

    impl WidgetImpl for CommandPalette {}
    impl BinImpl for CommandPalette {}
}

glib::wrapper! {
    pub struct CommandPalette(ObjectSubclass<imp::CommandPalette>)
        @extends libadwaita::Bin, gtk4::Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget;
}

impl CommandPalette {
    /// Create a new instance of the command palette. The caller will own a
    /// reference to the object.
    pub fn new() -> CommandPalette {
        let this: CommandPalette = glib::Object::new();

        // Sink ourselves so that we aren't floating anymore. We'll unref
        // ourselves when the palette is closed or an action is activated.
        this.imp().self_ref.replace(Some(this.clone()));

        // Bump the ref so that the caller has a reference.
        this
    }

    //---------------------------------------------------------------
    // Signal Handlers

    fn prop_config(&self) {
        let priv_ = self.imp();

        let Some(config) = priv_.config.borrow().clone() else {
            log::warn!(target: LOG, "command palette does not have a config!");
            return;
        };

        // Clear existing binds
        priv_.source.remove_all();

        let mut commands: Vec<Command> = Vec::new();

        self.collect_jump_commands(&config, &mut commands);
        self.collect_regular_commands(&config, &mut commands);

        // Sort commands
        commands.sort_by(|a, b| {
            if compare_commands(a, b) {
                Ordering::Less
            } else if compare_commands(b, a) {
                Ordering::Greater
            } else {
                Ordering::Equal
            }
        });

        for cmd in &commands {
            priv_.source.append(cmd);
        }
    }

    /// Collect regular commands from configuration, filtering out unsupported actions.
    fn collect_regular_commands(&self, config: &Config, commands: &mut Vec<Command>) {
        let entries: Vec<InputCommand> = config.get().command_palette_entry.value.clone();

        for command in entries {
            // Filter out actions that are not implemented or don't make sense
            // for GTK.
            if !is_action_supported_on_gtk(&command.action) {
                continue;
            }

            commands.push(Command::new(config, command));
        }
    }

    /// Collect jump commands for all surfaces across all windows.
    fn collect_jump_commands(&self, config: &Config, commands: &mut Vec<Command>) {
        let app = Application::default();

        // Get all surfaces from the core app
        for surface in app.surfaces() {
            commands.push(Command::new_jump(config, &surface));
        }
    }

    fn close(&self) {
        let _ = self.imp().dialog.close();
    }

    fn dialog_closed(&self) {
        // Drop the reference taken in `new`.
        let _self_ref = self.imp().self_ref.take();
    }

    //---------------------------------------------------------------

    /// Show or hide the command palette dialog. If the dialog is shown it will
    /// be modal over the given window.
    pub fn toggle(&self, window: &Window) {
        let priv_ = self.imp();

        // If the dialog has been shown, close it.
        if priv_.dialog.is_realized() {
            self.close();
            return;
        }

        // Show the dialog
        priv_.dialog.present(Some(window));

        // Focus on the search bar when opening the dialog
        let _ = priv_.search.grab_focus();
    }

    /// Helper function to send a signal containing the action that should be
    /// performed.
    fn activated(&self, pos: u32) {
        let priv_ = self.imp();

        // Use priv.model and not priv.source here to use the list of *visible* results
        let object = priv_.model.item(pos);

        // Close before running the action in order to avoid being replaced by
        // another dialog (such as the change title dialog). If that occurs then
        // the command palette dialog won't be counted as having closed properly
        // and cannot receive focus when reopened.
        self.close();

        let Some(object) = object else { return };
        let Ok(cmd) = object.downcast::<Command>() else { return };

        // Handle jump commands differently
        if cmd.is_jump() {
            let Some(surface) = cmd.get_jump_surface() else { return };
            surface.present();
            return;
        }

        // Regular command - emit trigger signal
        let Some(action) = cmd.get_action() else { return };

        // Signal that an action has been selected. Signals are synchronous
        // so we shouldn't need to worry about cloning the action.
        self.emit_by_name::<()>(signals::TRIGGER, &[&class::pointer_value(&action)]);
    }

    /// `signals.trigger.connect`
    pub fn connect_trigger<F: Fn(&Self, &Action) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_local(signals::TRIGGER, false, move |values| {
            let this: Self = values[0].get().unwrap();
            // SAFETY: the pointer is an `Action` that outlives the
            // synchronous emission.
            let action = unsafe { class::pointer_from_value::<Action>(&values[1]) };
            f(&this, action);
            None
        })
    }
}

impl Default for CommandPalette {
    fn default() -> Self {
        Self::new()
    }
}

/// Check if an action is supported on GTK.
fn is_action_supported_on_gtk(action: &Action) -> bool {
    !matches!(
        action,
        Action::CloseAllWindows
            | Action::ToggleSecureInput
            | Action::CheckForUpdates
            | Action::Redo
            | Action::Undo
            | Action::ResetWindowSize
            | Action::ToggleWindowFloatOnTop
    )
}

/// Compare two commands for sorting.
/// Sorts alphabetically by title (case-insensitive), with colon normalization
/// so "Foo:" sorts before "Foo Bar:". Uses sort_key as tie-breaker.
fn compare_commands(a: &Command, b: &Command) -> bool {
    let Some(a_title) = a.prop_get_title() else { return false };
    let Some(b_title) = b.prop_get_title() else { return true };
    let (a_title, b_title) = (a_title.as_bytes(), b_title.as_bytes());

    // Compare case-insensitively with colon normalization
    for i in 0..a_title.len().min(b_title.len()) {
        // Get characters, replacing ':' with '\t'
        let a_char = if a_title[i] == b':' { b'\t' } else { a_title[i] };
        let b_char = if b_title[i] == b':' { b'\t' } else { b_title[i] };

        let a_lower = a_char.to_ascii_lowercase();
        let b_lower = b_char.to_ascii_lowercase();

        if a_lower != b_lower {
            return a_lower < b_lower;
        }
    }

    // If one title is a prefix of the other, shorter one comes first
    if a_title.len() != b_title.len() {
        return a_title.len() < b_title.len();
    }

    // Titles are equal - use sort_key as tie-breaker if both are jump commands
    let Some(a_sort_key) = a.jump_sort_key() else { return false };
    let Some(b_sort_key) = b.jump_sort_key() else { return false };

    a_sort_key < b_sort_key
}

/// Property names of [`Command`].
pub mod command_properties {
    pub const CONFIG: &str = "config";
    pub const ACTION_KEY: &str = "action-key";
    pub const ACTION: &str = "action";
    pub const TITLE: &str = "title";
    pub const DESCRIPTION: &str = "description";
}

/// `Private.RegularData`
pub struct RegularData {
    pub command: InputCommand,
    pub action: Option<String>,
    pub action_key: Option<String>,
}

/// `Private.JumpData`
pub struct JumpData {
    pub surface: WeakRef<Surface>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub sort_key: usize,
}

/// `Private.CommandData`
pub enum CommandData {
    Regular(RegularData),
    Jump(JumpData),
}

mod command_imp {
    use super::*;

    #[derive(Default)]
    pub struct Command {
        pub config: RefCell<Option<Config>>,
        /// `None` only between instance init and `new`/`newJump`.
        pub data: RefCell<Option<CommandData>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Command {
        const NAME: &'static str = "GhosttyCommand";
        type Type = super::Command;
        type ParentType = glib::Object;
    }

    impl ObjectImpl for Command {
        fn properties() -> &'static [glib::ParamSpec] {
            use super::command_properties as p;
            static PROPS: LazyLock<Vec<glib::ParamSpec>> = LazyLock::new(|| {
                vec![
                    glib::ParamSpecObject::builder::<Config>(p::CONFIG).build(),
                    glib::ParamSpecString::builder(p::ACTION_KEY).read_only().build(),
                    glib::ParamSpecString::builder(p::ACTION).read_only().build(),
                    glib::ParamSpecString::builder(p::TITLE).read_only().build(),
                    glib::ParamSpecString::builder(p::DESCRIPTION).read_only().build(),
                ]
            });
            PROPS.as_ref()
        }

        fn property(&self, _: usize, pspec: &glib::ParamSpec) -> glib::Value {
            use super::command_properties as p;
            let obj = self.obj();
            match pspec.name() {
                p::CONFIG => accessor::get(&self.config),
                p::ACTION_KEY => obj.prop_get_action_key().to_value(),
                p::ACTION => obj.prop_get_action().to_value(),
                p::TITLE => obj.prop_get_title().to_value(),
                p::DESCRIPTION => obj.prop_get_description().to_value(),
                _ => unreachable!(),
            }
        }

        fn set_property(&self, _: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            match pspec.name() {
                super::command_properties::CONFIG => accessor::set(&self.config, value),
                _ => unreachable!(),
            }
        }

        fn dispose(&self) {
            self.config.replace(None);

            if let Some(CommandData::Jump(j)) = self.data.borrow_mut().as_mut() {
                j.surface.deinit();
            }
        }
    }
}

glib::wrapper! {
    /// Object that wraps around a command.
    ///
    /// As GTK list models only accept objects that are within the GObject hierarchy,
    /// we have to construct a wrapper to be easily consumed by the list model.
    pub struct Command(ObjectSubclass<command_imp::Command>);
}

impl Command {
    pub fn new(config: &Config, command: InputCommand) -> Command {
        let this: Command =
            glib::Object::builder().property(command_properties::CONFIG, config).build();
        this.imp().data.replace(Some(CommandData::Regular(RegularData {
            command,
            action: None,
            action_key: None,
        })));
        this
    }

    /// Create a new jump command that focuses a specific surface.
    pub fn new_jump(config: &Config, surface: &Surface) -> Command {
        let this: Command =
            glib::Object::builder().property(command_properties::CONFIG, config).build();

        // Surface should be initialized at this point.
        let core = surface.core().expect("jump surface must be initialized");
        // SAFETY: an initialized surface's core surface is live.
        let sort_key = unsafe { (*core).id } as usize;
        let mut jump = JumpData { surface: WeakRef::empty(), title: None, description: None, sort_key };
        jump.surface.set(Some(surface));
        this.imp().data.replace(Some(CommandData::Jump(jump)));
        this
    }

    fn jump_sort_key(&self) -> Option<usize> {
        match self.imp().data.borrow().as_ref()? {
            CommandData::Regular(_) => None,
            CommandData::Jump(j) => Some(j.sort_key),
        }
    }

    //---------------------------------------------------------------

    fn prop_get_action_key(&self) -> Option<String> {
        let mut data = self.imp().data.borrow_mut();
        let regular = match data.as_mut()? {
            CommandData::Regular(r) => r,
            CommandData::Jump(_) => return None,
        };

        if let Some(action_key) = &regular.action_key {
            return Some(action_key.clone());
        }

        regular.action_key = Some(regular.command.action.to_string());
        regular.action_key.clone()
    }

    fn prop_get_action(&self) -> Option<String> {
        let priv_ = self.imp();
        let mut data = priv_.data.borrow_mut();
        let regular = match data.as_mut()? {
            CommandData::Regular(r) => r,
            CommandData::Jump(_) => return None,
        };

        if let Some(action) = &regular.action {
            return Some(action.clone());
        }

        let config = priv_.config.borrow().clone()?;
        let cfg = config.get();
        let keybinds = &cfg.keybind.set;

        regular.action = (|| {
            let mut buf = [0u8; 64];
            let trigger = keybinds.get_trigger(&regular.command.action)?;
            let accel = key::accel_from_trigger(&mut buf, trigger).ok()??;
            Some(accel.to_string_lossy().into_owned())
        })();

        regular.action.clone()
    }

    fn prop_get_title(&self) -> Option<String> {
        let mut data = self.imp().data.borrow_mut();
        match data.as_mut()? {
            CommandData::Regular(r) => Some(r.command.title.to_string_lossy().into_owned()),
            CommandData::Jump(j) => {
                if let Some(title) = &j.title {
                    return Some(title.clone());
                }

                let surface = j.surface.get()?;

                let effective_title = surface
                    .get_effective_title()
                    .unwrap_or_else(|| i18n_(c"Untitled").to_owned());

                j.title = Some(format!("{}: {}", i18n_(c"Focus"), effective_title));
                j.title.clone()
            }
        }
    }

    fn prop_get_description(&self) -> Option<String> {
        let mut data = self.imp().data.borrow_mut();
        match data.as_mut()? {
            CommandData::Regular(r) => Some(r.command.description.to_string_lossy().into_owned()),
            CommandData::Jump(j) => {
                if let Some(desc) = &j.description {
                    return Some(desc.clone());
                }

                let surface = j.surface.get()?;

                let title = surface.get_effective_title().unwrap_or_else(|| "Untitled".to_owned());
                let pwd = surface.get_pwd();

                if let Some(p) = pwd {
                    if !title.contains(&p) {
                        j.description = Some(p);
                    }
                }

                j.description.clone()
            }
        }
    }

    //---------------------------------------------------------------

    /// Return a copy of the action. Callers must ensure that they do not use
    /// the action beyond the lifetime of this object because it has internally
    /// allocated data that will be freed when this object is.
    pub fn get_action(&self) -> Option<Action> {
        match self.imp().data.borrow().as_ref()? {
            CommandData::Regular(r) => Some(r.command.action.clone()),
            CommandData::Jump(_) => None,
        }
    }

    /// Check if this is a jump command.
    pub fn is_jump(&self) -> bool {
        matches!(self.imp().data.borrow().as_ref(), Some(CommandData::Jump(_)))
    }

    /// Get the jump surface. Returns a strong reference, or null if the
    /// surface has been destroyed.
    pub fn get_jump_surface(&self) -> Option<Surface> {
        match self.imp().data.borrow().as_ref()? {
            CommandData::Regular(_) => None,
            CommandData::Jump(j) => j.surface.get(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apprt::gtk::ext::test_gtk;
    use crate::config::config::Config as CoreConfig;
    use std::borrow::Cow;
    use std::rc::Rc;

    #[test]
    fn blueprint_path() {
        assert_eq!(class::blueprint(BLUEPRINT), "/com/mitchellh/ghostty/ui/1.5/command-palette.ui");
    }

    #[test]
    fn supported_actions() {
        assert!(!is_action_supported_on_gtk(&Action::Undo));
        assert!(!is_action_supported_on_gtk(&Action::CloseAllWindows));
        assert!(is_action_supported_on_gtk(&Action::ToggleCommandPalette));
    }

    fn cmd(config: &Config, title: &'static std::ffi::CStr) -> Command {
        Command::new(config, InputCommand::new(Action::Ignore, Cow::Borrowed(title)))
    }

    #[test]
    fn sort_order() {
        let config = Config::new(&CoreConfig::default());
        let a = cmd(&config, c"Foo:");
        let b = cmd(&config, c"Foo Bar:");
        let c = cmd(&config, c"foo");
        // ':' sorts like '\t', before ' '.
        assert!(compare_commands(&a, &b));
        assert!(!compare_commands(&b, &a));
        // Prefix sorts first.
        assert!(compare_commands(&c, &a));
        // Equal regular titles are not less.
        assert!(!compare_commands(&a, &cmd(&config, c"foo:")));
    }

    #[test]
    fn command_properties() {
        let config = Config::new(&CoreConfig::default());
        let c = Command::new(
            &config,
            InputCommand {
                action: Action::ToggleCommandPalette,
                title: Cow::Borrowed(c"Toggle Command Palette"),
                description: Cow::Borrowed(c"desc"),
            },
        );
        assert_eq!(c.property::<Option<String>>(command_properties::TITLE).as_deref(), Some("Toggle Command Palette"));
        assert_eq!(c.property::<Option<String>>(command_properties::DESCRIPTION).as_deref(), Some("desc"));
        assert_eq!(
            c.property::<Option<String>>(command_properties::ACTION_KEY).as_deref(),
            Some("toggle_command_palette")
        );
        // The default config binds the command palette.
        assert!(c.property::<Option<String>>(command_properties::ACTION).is_some());
        assert!(!c.is_jump());
        assert_eq!(c.get_action(), Some(Action::ToggleCommandPalette));
    }

    #[test]
    fn palette_and_jump_commands() {
        test_gtk::run(|| {
            let config = Config::new(&CoreConfig::default());

            // Jump commands need surfaces with a core surface (a running
            // terminal); they are covered by the GTK app smoke test in
            // `apprt/gtk/app.rs`.
            let n_surfaces = Application::default().surfaces().len();

            let palette = CommandPalette::new();
            palette.set_property(properties::CONFIG, &config);
            let n = palette.imp().source.n_items();
            assert!(n > 1);
            // Commands are sorted.
            let titles: Vec<String> = (0..n)
                .map(|i| {
                    palette.imp().source.item(i).unwrap().downcast::<Command>().unwrap()
                        .prop_get_title().unwrap()
                })
                .collect();
            assert_eq!(titles.iter().filter(|t| t.starts_with("Focus:")).count(), n_surfaces);

            // Activating a regular command emits trigger.
            let triggered = Rc::new(RefCell::new(None));
            let t = triggered.clone();
            palette.connect_trigger(move |_, a| *t.borrow_mut() = Some(a.clone()));
            let pos = titles.iter().position(|t| !t.starts_with("Focus:")).unwrap();
            palette.activated(pos as u32);
            assert!(triggered.borrow().is_some());

            // The palette drops its own reference when its dialog closes.
            assert!(palette.imp().self_ref.borrow().is_some());
            palette.dialog_closed();
            assert!(palette.imp().self_ref.borrow().is_none());
        });
    }
}
