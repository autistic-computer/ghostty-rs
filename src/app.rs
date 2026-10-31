//! Port of `src/App.zig`.
//!
//! STATUS: ported
//!
//! App is the primary GUI application for ghostty. This builds the window,
//! sets up the renderer, etc. The primary run loop is started by calling
//! the "run" function.
//!
//! Deviations:
//! * [`App::create`] returns a `Box` (Zig returns a stable `*App`); the
//!   mailbox queue is a field of the boxed app like in Zig.
//! * `Mailbox.Queue` is [`MailboxQueue`]; `Message.NewWindow` and
//!   `Message.OpenConfig` are [`NewWindow`] and [`OpenConfig`].
//! * Errors are the core surface [`Error`] (an anyhow-like wrapper) since
//!   the app forwards surface errors.
//! * `std.Io.Timestamp` (awake clock) is [`Instant`].
//! * The wasm API (`Wasm`, an empty struct on non-wasm targets with only
//!   commented-out code) has no counterpart.
//! * The font discovery and render device warmup threads are only spawned
//!   where those decls exist in Zig (CoreText / Metal, i.e. macOS); the
//!   Linux backends have none.

use std::time::Instant;

use crate::apprt;
use crate::apprt::action::{Action as RtAction, Target};
use crate::config::{conditional::Theme, Config, ConditionalState};
use crate::datastruct::BlockingQueue;
use crate::datastruct::blocking_queue::{Size as QueueSize, Timeout as QueueTimeout};
use crate::font::shared_grid_set::{InitError as SharedGridSetInitError, SharedGridSet};
use crate::input;
use crate::input::binding::{Action as BindingAction, ScopedApp, Value as BindingValue};
use crate::renderer;
use crate::surface::{Error, Surface};

const LOG: &str = "app";

type SurfaceList = Vec<*mut apprt::Surface>;

/// See the module documentation.
pub struct App {
    /// The list of surfaces that are currently active.
    pub surfaces: SurfaceList,

    /// This is true if the app that Ghostty is in is focused. This may
    /// mean that no surfaces (terminals) are focused but the app is still
    /// focused, i.e. may an about window. On macOS, this concept is known
    /// as the "active" app while focused windows are known as the
    /// "main" window.
    ///
    /// This is used to determine if keyboard shortcuts that are non-global
    /// should be processed. If the app is not focused, then we don't want
    /// to process keyboard shortcuts that are not global.
    ///
    /// This defaults to true since we assume that the app is focused when
    /// Ghostty is initialized but a well behaved apprt should call
    /// focusEvent to set this to the correct value right away.
    pub focused: bool,

    /// The last focused surface. This surface may not be valid;
    /// you must always call hasSurface to validate it.
    pub focused_surface: Option<*mut Surface>,

    /// The mailbox that can be used to send this thread messages. Note
    /// this is a blocking queue so if it is full you will get errors (or block).
    pub mailbox: MailboxQueue,

    /// The set of font GroupCache instances shared by surfaces with the
    /// same font configuration.
    pub font_grid_set: SharedGridSet,

    /// The app-scoped render device, shared by the renderers of all surfaces.
    pub device: Option<Box<renderer::Device>>,

    // Used to rate limit desktop notifications. Some platforms (notably macOS) will
    // run out of resources if desktop notifications are sent too fast and the OS
    // will kill Ghostty.
    pub last_notification_time: Option<Instant>,
    pub last_notification_digest: u64,

    /// The conditional state of the configuration. See the equivalent field
    /// in the Surface struct for more information. In this case, this applies
    /// to the app-level config and as a default for new surfaces.
    pub config_conditional_state: ConditionalState,

    /// Set to false once we've created at least one surface. This
    /// never goes true again. This can be used by surfaces to determine
    /// if they are the first surface.
    pub first: bool,
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("surfaces", &self.surfaces.len())
            .field("focused", &self.focused)
            .field("focused_surface", &self.focused_surface)
            .field("first", &self.first)
            .finish_non_exhaustive()
    }
}

/// `App.CreateError`
pub type CreateError = SharedGridSetInitError;

impl App {
    /// Create a new app instance. This returns a stable pointer to the app
    /// instance which is required for callbacks.
    pub fn create() -> Result<Box<App>, CreateError> {
        let app = Box::new(App::init()?);

        // If font discovery supports warmup, then we call it. Some font
        // mechanisms (e.g. CoreText) have a multi-millisecond one-time cost
        // on startup.
        //
        // Same for the renderer's graphics API (e.g. Metal), which pays
        // one-time framework initialization costs on first use.
        //
        // (Neither fontconfig nor OpenGL has a warmup decl.)

        Ok(app)
    }

    /// Initialize the main app instance. This creates the main window, sets
    /// up the renderer state, compiles the shaders, etc. This is the primary
    /// "startup" logic.
    ///
    /// After calling this function, well behaved apprts should then call
    /// `focusEvent` to set the initial focus state of the app.
    pub fn init() -> Result<App, CreateError> {
        let font_grid_set = SharedGridSet::init()?;

        Ok(App {
            surfaces: Vec::new(),
            focused: true,
            focused_surface: None,
            mailbox: MailboxQueue::new(),
            font_grid_set,
            device: None,
            last_notification_time: None,
            last_notification_digest: 0,
            config_conditional_state: ConditionalState::default(),
            first: true,
        })
    }

    pub fn deinit(&mut self) {
        // Clean up all our surfaces
        for surface in std::mem::take(&mut self.surfaces) {
            // SAFETY: surfaces in the list are live until deleted.
            unsafe { (*surface).deinit() };
        }

        // Clean up our font group cache
        // We should have zero items in the grid set at this point because
        // destroy only gets called when the app is shutting down and this
        // should gracefully close all surfaces.
        debug_assert!(self.font_grid_set.count() == 0);

        // Clean up our render device. This must happen after all surfaces
        // are gone since their renderers borrow it.
        if let Some(dev) = self.device.take() {
            dev.deinit();
        }
    }

    pub fn destroy(mut self: Box<Self>) {
        // Deinitialize the app
        self.deinit();

        // Free the app memory
        drop(self);
    }

    /// Tick ticks the app loop. This will drain our mailbox and process those
    /// events. This should be called by the application runtime on every loop
    /// tick.
    pub fn tick(&mut self, rt_app: &apprt::App) -> Result<(), Error> {
        // Drain our mailbox
        self.drain_mailbox(rt_app)
    }

    /// Update the configuration associated with the app. This can only be
    /// called from the main thread. The caller owns the config memory. The
    /// memory can be freed immediately when this returns.
    pub fn update_config(&mut self, rt_app: &apprt::App, config: &Config) -> Result<(), Error> {
        // Go through and update all of the surface configurations.
        for &surface in &self.surfaces {
            // SAFETY: surfaces in the list are live.
            let core = unsafe { (*surface).core() };
            unsafe { (*core).handle_message(apprt::surface::Message::ChangeConfig(config)) }?;
        }

        // Apply our conditional state. If we fail to apply the conditional state
        // then we log and attempt to move forward with the old config.
        // We only apply this to the app-level config because the surface
        // config applies its own conditional state.
        let applied_: Option<Config> = match config.change_conditional_state(self.config_conditional_state) {
            Ok(v) => v,
            Err(err) => {
                log::warn!(target: LOG, "failed to apply conditional state to config err={err}");
                None
            }
        };
        let applied: &Config = applied_.as_ref().unwrap_or(config);

        // Notify the apprt that the app has changed configuration.
        rt_app.perform_action(
            Target::App,
            RtAction::ConfigChange(apprt::action::ConfigChange { config: applied }),
        )?;

        Ok(())
    }

    /// Add an initialized surface. This is really only for the runtime
    /// implementations to call and should NOT be called by general app users.
    /// The surface must be from the pool.
    pub fn add_surface(&mut self, rt_surface: *mut apprt::Surface) {
        self.surfaces.push(rt_surface);

        // Since we have non-zero surfaces, we can cancel the quit timer.
        // It is up to the apprt if there is a quit timer at all and if it
        // should be canceled.
        // SAFETY: the runtime surface and its app are live.
        let rt_app = unsafe { &*(*rt_surface).rt_app() };
        if let Err(err) = rt_app.perform_action(
            Target::App,
            RtAction::QuitTimer(apprt::action::QuitTimer::Stop),
        ) {
            log::warn!(target: LOG, "error stopping quit timer err={err}");
        }
    }

    /// Delete the surface from the known surface list. This will NOT call the
    /// destructor or free the memory.
    pub fn delete_surface(&mut self, rt_surface: *mut apprt::Surface) {
        // If this surface is the focused surface then we need to clear it.
        // There was a bug where we relied on hasSurface to return false and
        // just let focused surface be but the allocator was reusing addresses
        // after free and giving false positives, so we must clear it.
        if let Some(focused) = self.focused_surface {
            // SAFETY: the runtime surface is live.
            if focused == unsafe { (*rt_surface).core() } {
                self.focused_surface = None;
            }
        }

        let mut i = 0;
        while i < self.surfaces.len() {
            if self.surfaces[i] == rt_surface {
                self.surfaces.swap_remove(i);
                continue;
            }

            i += 1;
        }

        // If we have no surfaces, we can start the quit timer. It is up to the
        // apprt to determine if this is necessary.
        if self.surfaces.is_empty() {
            // SAFETY: the runtime surface and its app are live.
            let rt_app = unsafe { &*(*rt_surface).rt_app() };
            if let Err(err) = rt_app.perform_action(
                Target::App,
                RtAction::QuitTimer(apprt::action::QuitTimer::Start),
            ) {
                log::warn!(target: LOG, "error starting quit timer err={err}");
            }
        }
    }

    /// The last focused surface. This is only valid while on the main thread
    /// before tick is called.
    pub fn focused_surface(&self) -> Option<*mut Surface> {
        let surface = self.focused_surface?;
        if !self.has_surface(surface) {
            return None;
        }
        Some(surface)
    }

    /// Returns true if confirmation is needed to quit the app. It is up to
    /// the apprt to call this.
    pub fn needs_confirm_quit(&self) -> bool {
        for &v in &self.surfaces {
            // SAFETY: surfaces in the list are live.
            if unsafe { (*(*v).core()).needs_confirm_quit() } {
                return true;
            }
        }

        false
    }

    /// Drain the mailbox.
    fn drain_mailbox(&mut self, rt_app: &apprt::App) -> Result<(), Error> {
        while let Some(message) = self.mailbox.pop() {
            log::debug!(target: LOG, "mailbox message={}", message.tag_name());
            match message {
                Message::OpenConfig(v) => self.perform_action(
                    rt_app,
                    ScopedApp::OpenConfig(match v {
                        OpenConfig::OsOpen => input::binding::OpenConfig::OsOpen,
                        OpenConfig::NewWindow => input::binding::OpenConfig::NewWindow,
                    }),
                )?,
                Message::NewWindow(msg) => self.new_window(rt_app, msg)?,
                Message::Close(surface) => self.close_surface(surface),
                Message::SurfaceMessage { surface, message } => self.surface_message(surface, message)?,

                // If we're quitting, then we set the quit flag and stop
                // draining the mailbox immediately. This lets us defer
                // mailbox processing to the next tick so that the apprt
                // can try to quit as quickly as possible.
                Message::Quit => {
                    log::info!(target: LOG, "quit message received, short circuiting mailbox drain");
                    self.perform_action(rt_app, ScopedApp::Quit)?;
                    return Ok(());
                }
            }
        }

        Ok(())
    }

    pub fn close_surface(&mut self, surface: *mut Surface) {
        if !self.has_surface(surface) {
            return;
        }
        // SAFETY: has_surface validated the pointer.
        unsafe { (*surface).close() };
    }

    pub fn focus_surface(&mut self, surface: *mut Surface) {
        if !self.has_surface(surface) {
            return;
        }
        self.focused_surface = Some(surface);
    }

    /// Create a new window
    pub fn new_window(&mut self, rt_app: &apprt::App, msg: NewWindow) -> Result<(), Error> {
        let target: Target = 'target: {
            let Some(parent) = msg.parent else {
                break 'target Target::App;
            };
            if self.has_surface(parent) {
                break 'target Target::Surface(parent);
            }
            Target::App
        };

        rt_app.perform_action(target, RtAction::NewWindow)?;
        Ok(())
    }

    /// Handle an app-level focus event. This should be called whenever
    /// the focus state of the entire app containing Ghostty changes.
    /// This is separate from surface focus events. See the `focused`
    /// field for more information.
    pub fn focus_event(&mut self, focused: bool) {
        // Prevent redundant focus events
        if self.focused == focused {
            return;
        }

        log::debug!(target: LOG, "focus event focused={focused}");
        self.focused = focused;
    }

    /// Handle a key event at the app-scope. If this key event is used,
    /// this will return true and the caller shouldn't continue processing
    /// the event. If the event is not used, this will return false.
    ///
    /// If the app currently has focus then all key events are processed.
    /// If the app does not have focus then only global key events are
    /// processed.
    ///
    /// Only runtimes with an app-level `config` (the embedded runtime and the
    /// recording stand-in) call this; Zig compiles it lazily, so it is not
    /// built with the GTK runtime.
    #[cfg(any(test, not(feature = "gtk")))]
    pub fn key_event(&mut self, rt_app: &apprt::App, event: input::KeyEvent<'_>) -> bool {
        match event.action {
            // We don't care about key release events.
            input::Action::Release => return false,

            // Continue processing key press events.
            input::Action::Press | input::Action::Repeat => {}
        }

        // Get the keybind entry for this event. We don't support key sequences
        // so we can look directly in the top-level set.
        let Some(entry) = rt_app.config.keybind.set.get_event(&event) else {
            return false;
        };
        let leaf = match entry.value_ptr {
            // Sequences aren't supported. Our configuration parser verifies
            // this for global keybinds but we may still get an entry for
            // a non-global keybind.
            BindingValue::Leader(_) => return false,

            // Leaf entries are good
            BindingValue::Leaf(leaf) => leaf.generic(),
            BindingValue::LeafChained(leaf) => leaf.generic(),
        };
        let actions: &[BindingAction] = leaf.actions_slice();
        debug_assert!(!actions.is_empty());

        // If we aren't focused, then we only process global keybinds.
        if !self.focused && !leaf.flags.global {
            return false;
        }

        // Global keybinds are done using performAll so that they
        // can target all surfaces too.
        if leaf.flags.global {
            self.perform_all_chained_action(rt_app, actions);
            return true;
        }

        // Must be focused to process non-global keybinds
        debug_assert!(self.focused);
        debug_assert!(!leaf.flags.global);

        // If we are focused, then we process keybinds only if they are
        // app-scoped. Otherwise, we do nothing. Surface-scoped should
        // be processed by Surface.keyEvent. For chained actions, all
        // actions must be app-scoped.
        for action in actions {
            if action.clone().scoped_app().is_none() {
                return false;
            }
        }
        for action in actions {
            if let Err(err) = self.perform_action(rt_app, action.clone().scoped_app().unwrap()) {
                log::warn!(
                    target: LOG,
                    "error performing app keybind action action={} err={err}",
                    action.key()
                );
            }
        }

        true
    }

    /// Call to notify Ghostty that the color scheme for the app has changed.
    /// "Color scheme" in this case refers to system themes such as "light/dark".
    pub fn color_scheme_event(&mut self, rt_app: &apprt::App, scheme: apprt::ColorScheme) -> Result<(), Error> {
        let new_scheme = match scheme {
            apprt::ColorScheme::Light => Theme::Light,
            apprt::ColorScheme::Dark => Theme::Dark,
        };

        // If our scheme didn't change, then we don't do anything.
        if self.config_conditional_state.theme == new_scheme {
            return Ok(());
        }

        // Setup our conditional state which has the current color theme.
        self.config_conditional_state.theme = new_scheme;

        // Request our configuration be reloaded because the new scheme may
        // impact the colors of the app.
        rt_app.perform_action(
            Target::App,
            RtAction::ReloadConfig(apprt::action::ReloadConfig { soft: true }),
        )?;

        Ok(())
    }

    /// Perform a binding action. This only accepts actions that are scoped
    /// to the app. Callers can use performAllAction to perform any action
    /// and any non-app-scoped actions will be performed on all surfaces.
    pub fn perform_action(&mut self, rt_app: &apprt::App, action: ScopedApp) -> Result<(), Error> {
        match action {
            ScopedApp::Unbind => unreachable!(),
            ScopedApp::Ignore => {}
            ScopedApp::Quit => {
                rt_app.perform_action(Target::App, RtAction::Quit)?;
            }
            ScopedApp::NewWindow => self.new_window(rt_app, NewWindow { parent: None })?,
            ScopedApp::OpenConfig(v) => {
                rt_app.perform_action(
                    Target::App,
                    RtAction::OpenConfig(match v {
                        input::binding::OpenConfig::OsOpen => apprt::action::OpenConfig::OsOpen,
                        input::binding::OpenConfig::NewWindow => apprt::action::OpenConfig::NewWindow,
                    }),
                )?;
            }
            ScopedApp::ReloadConfig => {
                rt_app.perform_action(
                    Target::App,
                    RtAction::ReloadConfig(apprt::action::ReloadConfig::default()),
                )?;
            }
            ScopedApp::CloseAllWindows => {
                rt_app.perform_action(Target::App, RtAction::CloseAllWindows)?;
            }
            ScopedApp::ToggleQuickTerminal => {
                rt_app.perform_action(Target::App, RtAction::ToggleQuickTerminal)?;
            }
            ScopedApp::ToggleVisibility => {
                rt_app.perform_action(Target::App, RtAction::ToggleVisibility)?;
            }
            ScopedApp::CheckForUpdates => {
                rt_app.perform_action(Target::App, RtAction::CheckForUpdates)?;
            }
            ScopedApp::ShowGtkInspector => {
                rt_app.perform_action(Target::App, RtAction::ShowGtkInspector)?;
            }
            ScopedApp::Undo => {
                rt_app.perform_action(Target::App, RtAction::Undo)?;
            }

            ScopedApp::Redo => {
                rt_app.perform_action(Target::App, RtAction::Redo)?;
            }
        }

        Ok(())
    }

    /// Performs a chained action. We will continue executing each action
    /// even if there is a failure in a prior action.
    pub fn perform_all_chained_action(&mut self, rt_app: &apprt::App, actions: &[BindingAction]) {
        for action in actions {
            if let Err(err) = self.perform_all_action(rt_app, action.clone()) {
                log::warn!(
                    target: LOG,
                    "error performing chained action action={} err={err}",
                    action.key()
                );
            }
        }
    }

    /// Perform an app-wide binding action. If the action is surface-specific
    /// then it will be performed on all surfaces. To perform only app-scoped
    /// actions, use performAction.
    pub fn perform_all_action(&mut self, rt_app: &apprt::App, action: BindingAction) -> Result<(), Error> {
        match action.scope() {
            // App-scoped actions are handled by the app so that they aren't
            // repeated for each surface (since each surface forwards
            // app-scoped actions back up).
            input::binding::Scope::App => self.perform_action(
                rt_app,
                action.scoped_app().unwrap(), // asserted through the scope match
            )?,

            // Surface-scoped actions are performed on all surfaces. Errors
            // are logged but processing continues.
            input::binding::Scope::Surface => {
                for surface in self.surfaces.clone() {
                    // SAFETY: surfaces in the list are live.
                    let core = unsafe { &mut *(*surface).core() };
                    if let Err(err) = core.perform_binding_action(action.clone()) {
                        log::warn!(
                            target: LOG,
                            "error performing binding action on surface id={:x} err={err}",
                            core.id
                        );
                    }
                }
            }
        }

        Ok(())
    }

    /// Handle a window message
    fn surface_message(&mut self, surface: *mut Surface, msg: apprt::surface::Message) -> Result<(), Error> {
        // We want to ensure our window is still active. Window messages
        // are quite rare and we normally don't have many windows so we do
        // a simple linear search here.
        if self.has_surface(surface) {
            // SAFETY: has_surface validated the pointer.
            unsafe { (*surface).handle_message(msg) }?;
        } else {
            // Window was not found, it probably quit before we handled the message.
            // Not a problem. (Owned payloads are freed on drop.)
            drop(msg);
        }

        Ok(())
    }

    fn has_surface(&self, surface: *const Surface) -> bool {
        for &v in &self.surfaces {
            // SAFETY: surfaces in the list are live.
            if unsafe { (*v).core() } as *const Surface == surface {
                return true;
            }
        }

        false
    }

    /// Search for a surface by a 64 bit unique ID.
    pub fn find_surface_by_id(&self, id: u64) -> Option<*mut Surface> {
        for &v in &self.surfaces {
            // SAFETY: surfaces in the list are live.
            let surface: *mut Surface = unsafe { (*v).core() };
            if unsafe { (*surface).id } == id {
                return Some(surface);
            }
        }

        None
    }
}

/// The message types that can be sent to the app thread.
#[derive(Debug)]
pub enum Message {
    // Open the configuration file
    OpenConfig(OpenConfig),

    /// Create a new terminal window.
    NewWindow(NewWindow),

    /// Close a surface. This notifies the runtime that a surface
    /// should close.
    Close(*mut Surface),

    /// Quit
    Quit,

    /// A message for a specific surface.
    SurfaceMessage {
        surface: *mut Surface,
        message: apprt::surface::Message,
    },
}

// SAFETY: the surface pointers are only identifiers validated (and
// dereferenced) on the app thread; the surface message payload is Send.
unsafe impl Send for Message {}

impl Message {
    /// The Zig tag name (for logging).
    pub fn tag_name(&self) -> &'static str {
        match self {
            Message::OpenConfig(_) => "open_config",
            Message::NewWindow(_) => "new_window",
            Message::Close(_) => "close",
            Message::Quit => "quit",
            Message::SurfaceMessage { .. } => "surface_message",
        }
    }
}

/// `Message.NewWindow`
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NewWindow {
    /// The parent surface
    pub parent: Option<*mut Surface>,
}

/// `Message.OpenConfig`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OpenConfig {
    /// Open the config in the OS default editor.
    OsOpen,
    /// Open the config in a new window using $EDITOR or $VISUAL
    NewWindow,
}

/// The type used for sending messages to the app thread (Zig:
/// `Mailbox.Queue`).
pub type MailboxQueue = BlockingQueue<Message, 64>;

/// Mailbox is the way that other threads send the app thread messages.
#[derive(Debug, Clone, Copy)]
pub struct Mailbox {
    pub rt_app: *const apprt::App,
    pub mailbox: *const MailboxQueue,
}

// SAFETY: the mailbox is a pair of pointers to thread-safe objects (the
// blocking queue and the runtime app's wakeup) that outlive every
// mailbox copy, exactly like the Zig struct.
unsafe impl Send for Mailbox {}
unsafe impl Sync for Mailbox {}

impl Mailbox {
    /// Send a message to the surface.
    pub fn push(&self, msg: Message, timeout: QueueTimeout) -> QueueSize {
        // SAFETY: see the Send/Sync impls above.
        let result = unsafe { (*self.mailbox).push(msg, timeout) };

        // Wake up our app loop
        unsafe { (*self.rt_app).wakeup() };

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Not Zig tests (App.zig has none): the app-level logic that does not
    // need a surface.

    #[test]
    fn focus_and_color_scheme_events() {
        let rt_app = apprt::App::default();
        let mut app = App::create().unwrap();
        assert!(app.focused);
        app.focus_event(false);
        assert!(!app.focused);

        app.color_scheme_event(&rt_app, apprt::ColorScheme::Dark).unwrap();
        assert_eq!(Theme::Dark, app.config_conditional_state.theme);
        // Unchanged scheme does nothing.
        app.color_scheme_event(&rt_app, apprt::ColorScheme::Dark).unwrap();
        let actions = rt_app.take_actions();
        assert_eq!(vec![apprt::action::ActionKey::ReloadConfig], actions);
        app.destroy();
    }

    #[test]
    fn mailbox_quit_short_circuits() {
        let rt_app = apprt::App::default();
        let mut app = App::create().unwrap();
        let mb = Mailbox {
            rt_app: &rt_app,
            mailbox: &app.mailbox,
        };
        mb.push(Message::Quit, QueueTimeout::Instant);
        mb.push(Message::NewWindow(NewWindow::default()), QueueTimeout::Instant);
        app.tick(&rt_app).unwrap();
        assert_eq!(vec![apprt::action::ActionKey::Quit], rt_app.take_actions());
        app.tick(&rt_app).unwrap();
        assert_eq!(vec![apprt::action::ActionKey::NewWindow], rt_app.take_actions());
        assert!(rt_app.wakeups() >= 2);
        app.destroy();
    }

    #[test]
    fn key_event_app_scoped_binding() {
        use crate::cli::args::ParseCli as _;
        let mut rt_app = apprt::App::default();
        rt_app
            .config
            .keybind
            .parse_cli(Some("ctrl+shift+q=quit"))
            .unwrap();
        let mut app = App::create().unwrap();
        let mut ev = input::KeyEvent {
            key: input::Key::KeyQ,
            unshifted_codepoint: 'q' as u32,
            mods: input::Mods {
                ctrl: true,
                shift: true,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(app.key_event(&rt_app, ev));
        assert_eq!(vec![apprt::action::ActionKey::Quit], rt_app.take_actions());

        // Releases are ignored.
        ev.action = input::Action::Release;
        assert!(!app.key_event(&rt_app, ev));

        // Unfocused apps only handle global binds.
        ev.action = input::Action::Press;
        app.focus_event(false);
        assert!(!app.key_event(&rt_app, ev));
        assert!(rt_app.take_actions().is_empty());
        app.destroy();
    }
}
