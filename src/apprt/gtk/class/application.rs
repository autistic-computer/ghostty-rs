//! Port of `src/apprt/gtk/class/application.zig`.
//!
//! STATUS: ported
//!
//! Rust notes / deviations:
//! * The GTK/libadwaita minimum version checks (`gtk_version.logVersion`,
//!   `adw_version.logVersion`) only warn instead of failing startup: the
//!   Rust port targets the gtk-rs 4.14 / libadwaita 1.5 bindings and is
//!   run on systems older than Ghostty's 4.18 / 1.7 minimum. Newer APIs are
//!   behind runtime version checks.
//! * The compiled resources are registered by `class::ensure_resources`
//!   (Zig does it in `Class.init`).
//! * Private state lives in `Cell`/`RefCell` fields; the core and runtime
//!   apps are raw pointers like Zig. `deinit` (the forced teardown) runs
//!   from `App::terminate` and is idempotent.
//! * `libxev` is not used by the Rust port, so `startupXev` only logs.
//! * In test builds, [`Application::default`] lazily creates an
//!   application (with a core app and the recording runtime app) when none
//!   exists, so widget smoke tests can instantiate classes that read the
//!   default application.
#![cfg(feature = "gtk")]

use std::cell::{Cell, RefCell};
use std::ffi::CStr;
use std::fmt::Write as _;
use std::sync::LazyLock;

use glib::prelude::*;
use glib::translate::IntoGlib;
use gtk4::gdk;
use gtk4::gio;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use libadwaita::prelude::*;
use libadwaita::subclass::prelude::*;

use crate::app::{self as core_app_mod, App as CoreApp};
use crate::surface::Error as CoreAppError;
use crate::apprt;
use crate::apprt::action::{
    self as rt_action, Action as RtAction, CloseTabMode, GotoSplit, GotoTab, GotoWindow, OpenUrl, OpenUrlKind,
    QuitTimer, SplitDirection, Target,
};
use crate::apprt::gtk::adw_version;
use crate::apprt::gtk::build::info as build_info;
use crate::apprt::gtk::class::close_confirmation_dialog::{self, CloseConfirmationDialog};
use crate::apprt::gtk::class::config_errors_dialog::ConfigErrorsDialog;
use crate::apprt::gtk::class::global_shortcuts::{BindFailed, GlobalShortcuts};
use crate::apprt::gtk::class::overrides::Overrides;
use crate::apprt::gtk::class::split_tree::SplitTree;
use crate::apprt::gtk::class::surface::{Size, Surface};
use crate::apprt::gtk::class::tab::Tab;
use crate::apprt::gtk::class::window::{SelectTab, Window};
use crate::apprt::gtk::class::{self, accessor, Config};
use crate::apprt::gtk::ext;
use crate::apprt::gtk::ext::actions::Action as GtkAction;
use crate::apprt::gtk::gtk_version;
use crate::apprt::gtk::key;
use crate::apprt::gtk::media;
use crate::apprt::gtk::portal::OpenURI;
use crate::apprt::gtk::winproto;
use crate::apprt::surface::ChildExited;
use crate::config::config::{AsyncBackend, Color as ConfigColor, Command, GtkSingleInstance, Path as ConfigPath, WindowTheme};
use crate::config::Config as CoreConfig;
use crate::datastruct::blocking_queue::Timeout as QueueTimeout;
use crate::datastruct::split_tree::{Direction, Goto, SpatialDirection};
use crate::global;
use crate::input::binding::{Action as BindingAction, ScopedApp};
use crate::os as internal_os;
use crate::os::systemd;
use crate::surface::Surface as CoreSurface;

const LOG: &str = "gtk_ghostty_application";

/// Function used to funnel GLib/GObject/GTK log messages into Rust's
/// logging system rather than just getting dumped directly to stderr.
fn glib_log_writer_function(level: glib::LogLevel, fields: &[glib::LogField<'_>]) -> glib::LogWriterOutput {
    const GLIB_LOG: &str = "glib";

    let mut message: Option<String> = None;
    let mut domain: Option<String> = None;
    for field in fields {
        let k = field.key();
        let Some(v) = field.value_str().map(str::to_owned).or_else(|| {
            field.value_bytes().map(|b| String::from_utf8_lossy(b).into_owned())
        }) else {
            continue;
        };
        if k == "MESSAGE" {
            message = Some(v);
            continue;
        }
        if k == "GLIB_DOMAIN" {
            domain = Some(v);
            continue;
        }
    }

    let Some(message) = message else { return glib::LogWriterOutput::Unhandled };
    let domain = domain.unwrap_or_else(|| "«unknown»".to_owned());

    match level {
        glib::LogLevel::Error => log::error!(target: GLIB_LOG, "ERROR: {domain}: {message}"),
        glib::LogLevel::Critical => log::error!(target: GLIB_LOG, "CRITICAL: {domain}: {message}"),
        glib::LogLevel::Warning => log::warn!(target: GLIB_LOG, "WARNING: {domain}: {message}"),
        glib::LogLevel::Message => log::info!(target: GLIB_LOG, "MESSAGE: {domain}: {message}"),
        glib::LogLevel::Info => log::info!(target: GLIB_LOG, "INFO: {domain}: {message}"),
        glib::LogLevel::Debug => log::debug!(target: GLIB_LOG, "DEBUG: {domain}: {message}"),
    }
    glib::LogWriterOutput::Handled
}

/// Property names (Zig: `properties`).
pub mod properties {
    pub const CONFIG: &str = "config";
}

/// The quit timer state.
#[derive(Debug, Default)]
pub enum QuitTimerState {
    #[default]
    Off,
    Active(glib::SourceId),
    Expired,
}

/// Errors from [`Application::new`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewError {
    GtkMinimumVersion,
    AdwaitaMinimumVersion,
}

impl std::fmt::Display for NewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for NewError {}

/// Errors from [`Application::run`].
#[derive(Debug)]
pub enum RunError {
    ContextAcquireFailed,
    ApplicationRegisterFailed,
    Tick(CoreAppError),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for RunError {}

mod imp {
    use super::*;

    pub struct Application {
        /// The apprt App. This is annoying that we need this it'd be
        /// nicer to just make THIS the apprt app but the current libghostty
        /// API doesn't allow that.
        pub rt_app: Cell<*mut apprt::App>,

        /// The libghostty App instance.
        pub core_app: Cell<*mut CoreApp>,

        /// The configuration for the application.
        pub config: RefCell<Option<Config>>,

        /// State and logic for the underlying windowing protocol.
        pub winproto: RefCell<Option<winproto::App>>,

        /// The global shortcut logic.
        pub global_shortcuts: RefCell<Option<GlobalShortcuts>>,

        /// This is set to true so long as we request a window exactly
        /// once. This prevents quitting the app before we've shown one
        /// window.
        pub requested_window: Cell<bool>,

        /// This is set to false internally when the event loop
        /// should exit and the application should quit. This must
        /// only be set by the main loop thread.
        pub running: Cell<bool>,

        /// True once we've told systemd that startup is complete. Reloads
        /// before then (e.g. syncing the color scheme during startup) must
        /// not notify systemd, or it sees READY=1 before we're ready.
        pub systemd_ready: Cell<bool>,

        /// The timer used to quit the application after the last window is
        /// closed. Even if there is no quit delay set, this is the state
        /// used to determine to close the app.
        pub quit_timer: RefCell<QuitTimerState>,

        /// If non-null, we're currently showing a config errors dialog.
        /// This is a WeakRef because the dialog can close on its own
        /// outside of our own lifecycle and that's okay.
        pub config_errors_dialog: glib::WeakRef<ConfigErrorsDialog>,

        /// glib source for our signal handler.
        pub signal_source: RefCell<Option<glib::SourceId>>,

        /// CSS Provider for any styles based on Ghostty configuration
        /// values.
        pub css_provider: RefCell<Option<gtk4::CssProvider>>,

        /// Providers for loading custom stylesheets defined by user
        pub custom_css_providers: RefCell<Vec<gtk4::CssProvider>>,

        /// A copy of the LANG environment variable that was provided to
        /// Ghostty by the system. If this is null, the LANG environment
        /// variable did not exist in Ghostty's environment variable.
        pub saved_language: RefCell<Option<String>>,

        pub open_uri: RefCell<Option<OpenURI>>,

        // The audio bell's MediaFile, reused across bells so we don't leak
        // a GStreamer pipeline (and its GL threads) on every ring. Built
        // lazily on the first audio bell and rebuilt when
        // `bell-audio-path` changes; unref'd on dispose. See ringBell and
        // media.zig.
        pub bell_media: RefCell<Option<gtk4::MediaFile>>,

        /// Whether `deinit` already ran.
        pub deinited: Cell<bool>,
    }

    impl Default for Application {
        fn default() -> Self {
            Application {
                rt_app: Cell::new(std::ptr::null_mut()),
                core_app: Cell::new(std::ptr::null_mut()),
                config: RefCell::new(None),
                winproto: RefCell::new(None),
                global_shortcuts: RefCell::new(None),
                requested_window: Cell::new(false),
                running: Cell::new(false),
                systemd_ready: Cell::new(false),
                quit_timer: RefCell::new(QuitTimerState::Off),
                config_errors_dialog: glib::WeakRef::new(),
                signal_source: RefCell::new(None),
                css_provider: RefCell::new(None),
                custom_css_providers: RefCell::new(Vec::new()),
                saved_language: RefCell::new(None),
                open_uri: RefCell::new(None),
                bell_media: RefCell::new(None),
                deinited: Cell::new(false),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Application {
        const NAME: &'static str = "GhosttyApplication";
        type Type = super::Application;
        type ParentType = libadwaita::Application;

        fn class_init(_klass: &mut Self::Class) {
            // Register our compiled resources exactly once.
            class::ensure_resources();
        }
    }

    impl ObjectImpl for Application {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPS: LazyLock<Vec<glib::ParamSpec>> = LazyLock::new(|| {
                vec![glib::ParamSpecObject::builder::<Config>(properties::CONFIG)
                    .read_only()
                    .build()]
            });
            PROPS.as_ref()
        }

        fn property(&self, _: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                properties::CONFIG => accessor::get(&self.config),
                _ => unreachable!(),
            }
        }

        fn dispose(&self) {
            if let Some(diag) = self.config_errors_dialog.upgrade() {
                diag.close();
            }
            self.config_errors_dialog.set(None);
            if let Some(v) = self.signal_source.take() {
                v.remove();
            }

            self.bell_media.replace(None);
        }
    }

    impl Drop for Application {
        // Zig: `finalize` calls `deinit`. The Rust `deinit` releases the
        // same resources, which the private struct's fields also drop.
        fn drop(&mut self) {}
    }

    impl ApplicationImpl for Application {
        fn startup(&self) {
            log::debug!(target: LOG, "startup");

            self.parent_startup();
            self.obj().startup();
        }

        fn activate(&self) {
            log::debug!(target: LOG, "activate");

            // Queue a new window
            self.obj().core().mailbox.push(
                core_app_mod::Message::NewWindow(core_app_mod::NewWindow::default()),
                QueueTimeout::Forever,
            );

            // Call the parent activate method.
            self.parent_activate();
        }
    }

    impl GtkApplicationImpl for Application {}
    impl AdwApplicationImpl for Application {}
}

glib::wrapper! {
    /// The primary entrypoint for the Ghostty GTK application.
    ///
    /// This requires a `ghostty.App` and `ghostty.Config` and takes
    /// care of the rest. Call `run` to run the application to completion.
    pub struct Application(ObjectSubclass<imp::Application>)
        @extends libadwaita::Application, gtk4::Application, gio::Application,
        @implements gio::ActionGroup, gio::ActionMap;
}

impl Application {
    /// Get this application as the default, allowing access to its
    /// properties globally.
    ///
    /// This asserts that there is a default application and that the
    /// default application is a GhosttyApplication. The program would have
    /// to be in a very bad state for this to be violated.
    pub fn default() -> Application {
        if let Some(app) = Self::try_default() {
            return app;
        }

        #[cfg(test)]
        {
            test_support::default_app()
        }

        #[cfg(not(test))]
        panic!("there is no default GhosttyApplication")
    }

    /// The default application, if there is one.
    pub fn try_default() -> Option<Application> {
        gio::Application::default()?.downcast::<Application>().ok()
    }

    /// Creates a new Application instance.
    ///
    /// This does a lot more work than a typical class instantiation,
    /// because we expect that this is the main program entrypoint.
    ///
    /// The only failure mode of initializing the application is early OOM.
    /// Early OOM can't be recovered from. Every other error is mapped to
    /// some degraded state where we can at least show a window with an
    /// error.
    pub fn new(core_app: &mut CoreApp) -> Result<Application, NewError> {
        // Capture GLib/GObject/GTK log messages and funnel them through our
        // logging system rather than just getting dumped directly to stderr.
        glib::log_set_writer_func(glib_log_writer_function);

        // Load our configuration.
        let config = match CoreConfig::load() {
            Ok(c) => c,
            Err(err) => {
                // If we fail to load the configuration, then we should log
                // the error in the diagnostics so it can be shown to the
                // user. We can still load a default which only fails for
                // OOM, allowing us to startup.
                let mut def = CoreConfig::default();
                def.add_diagnostic_fmt(format_args!("error loading user configuration: {err}"));
                def
            }
        };

        // Set the old language,
        let saved_language: Option<String> = {
            let old_language = global::environ().get("LANG").cloned();
            if let Some(language) = &config.language {
                // Override LANG if we need to (sync global environs if so)
                std::env::set_var("LANG", language.as_str());
                global::sync_environ();
            }
            old_language
        };

        // Set gettext global domain to be our app so that our unqualified
        // translations map to our translations.
        if let Err(err) = internal_os::i18n::init_global_domain() {
            // Failures shuldn't stop application startup. Our app may
            // not translate correctly but it should still work. In the
            // future we may want to add this to the GUI to show.
            log::warn!(target: LOG, "i18n initialization failed error={err:?}");
        }

        // Setup our GTK init env vars
        set_gtk_env(&config);

        // Initialize GTK (Zig's adw.init initializes GTK too).
        if let Err(err) = gtk4::init() {
            log::warn!(target: LOG, "gtk initialization failed err={err}");
        }
        let _ = libadwaita::init();

        // Log our GTK versions. The Rust port warns instead of failing
        // when the runtime is older than Ghostty's minimum (see the module
        // documentation).
        if gtk_version::log_version().is_err() {
            log::warn!(target: LOG, "GTK version is older than the supported minimum, some features may not work");
        }
        if adw_version::log_version().is_err() {
            log::warn!(target: LOG, "libadwaita version is older than the supported minimum, some features may not work");
        }

        Ok(Self::new_with_config(core_app, config, saved_language))
    }

    /// The part of [`Application::new`] after GTK initialization.
    fn new_with_config(core_app: &mut CoreApp, config: CoreConfig, saved_language: Option<String>) -> Application {
        let single_instance = match config.gtk_single_instance {
            GtkSingleInstance::True => true,
            GtkSingleInstance::False => false,
            // This should have been resolved to true/false during config
            // loading.
            GtkSingleInstance::Detect => false,
        };

        // Setup the flags for our application.
        let mut app_flags = gio::ApplicationFlags::empty();
        if !single_instance {
            app_flags |= gio::ApplicationFlags::NON_UNIQUE;
        }

        // Our app ID determines uniqueness and maps to our desktop file.
        // We append "-debug" to the ID if we're in debug mode so that we
        // can develop Ghostty in Ghostty.
        let app_id: String = 'app_id: {
            if let Some(class) = &config.class {
                if gio::Application::id_is_valid(class.as_str()) {
                    break 'app_id class.as_str().to_owned();
                } else {
                    log::warn!(target: LOG, "invalid 'class' in config, ignoring");
                }
            }

            build_info::APPLICATION_ID.to_owned()
        };

        let Some(display) = gdk::Display::default() else {
            // I'm unsure of any scenario where this happens. Because we
            // don't want to litter null checks everywhere, we just exit here.
            log::warn!(target: LOG, "gdk display is null, exiting");
            std::process::exit(1);
        };

        // Setup our windowing protocol logic
        let wp = match winproto::App::init(&display, &app_id, &config) {
            Ok(wp) => wp,
            Err(err) => {
                // If we fail to detect or setup the windowing protocol
                // specifies, we fallback to a noop implementation so we can
                // still launch.
                log::warn!(target: LOG, "error initializing windowing protocol err={err:?}");
                winproto::App::None(winproto::noop::App {})
            }
        };
        log::debug!(target: LOG, "windowing protocol={:?}", wp.protocol());

        // Create our GTK Application which encapsulates our process.
        log::debug!(target: LOG, "creating GTK application id={app_id} single-instance={single_instance}");

        // Wrap our configuration in a GObject.
        let config_obj = Config::new(&config);

        // Internally, GTK ensures that only one instance of this provider
        // exists in the provider list for the display.
        let css_provider = gtk4::CssProvider::new();
        gtk4::style_context_add_provider_for_display(
            &display,
            &css_provider,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION + 3,
        );

        // Initialize the app.
        let this: Application = glib::Object::builder()
            .property("application-id", &app_id)
            .property("flags", app_flags)
            // Force the resource path to a known value so it doesn't depend
            // on the app id (which changes between debug/release and can be
            // user-configured) and force it to load in compiled resources.
            .property("resource-base-path", build_info::RESOURCE_PATH)
            .build();

        // Setup our private state. More setup is done in the init
        // callback that GObject calls, but we can't pass this data through
        // to there (and we don't need it there directly) so this is here.
        let priv_ = this.imp();
        priv_.core_app.set(core_app);
        priv_.config.replace(Some(config_obj));
        priv_.winproto.replace(Some(wp));
        priv_.css_provider.replace(Some(css_provider.clone()));
        priv_.global_shortcuts.replace(Some(glib::Object::new::<GlobalShortcuts>()));
        priv_.saved_language.replace(saved_language);
        priv_.open_uri.replace(Some(OpenURI::init(Box::new(|kind, url| {
            Application::default().open_url_fallback(kind, url)
        }))));

        // Signals
        let weak = this.downgrade();
        this.connect_notify_local(Some(properties::CONFIG), move |_, _| {
            if let Some(this) = weak.upgrade() {
                this.prop_config();
            }
        });

        css_provider.connect_parsing_error(|_, section, err| signal_css_parsing_error(section, err));

        // Trigger initial config changes
        this.notify(properties::CONFIG);

        this
    }

    /// Set the runtime app (Zig sets it in `new`; the Rust runtime app
    /// wraps this application so it is created after it).
    pub fn set_rt(&self, rt_app: *mut apprt::App) {
        self.imp().rt_app.set(rt_app);
    }

    /// Force deinitialize the application.
    ///
    /// Normally in a GObject lifecycle, this would be called by the
    /// finalizer. But applications are never fully unreferenced so this
    /// ensures that our memory is cleaned up properly.
    pub fn deinit(&self) {
        let priv_ = self.imp();
        if priv_.deinited.replace(true) {
            return;
        }
        priv_.config.replace(None);
        if let Some(mut wp) = priv_.winproto.take() {
            wp.deinit();
        }
        if let Some(open_uri) = priv_.open_uri.take() {
            open_uri.deinit();
        }
        priv_.global_shortcuts.replace(None);
        priv_.saved_language.replace(None);
        if let Some(display) = gdk::Display::default() {
            if let Some(p) = priv_.css_provider.borrow().as_ref() {
                gtk4::style_context_remove_provider_for_display(&display, p);
            }

            for provider in priv_.custom_css_providers.borrow().iter() {
                gtk4::style_context_remove_provider_for_display(&display, provider);
            }
        }
        priv_.css_provider.replace(None);
        priv_.custom_css_providers.borrow_mut().clear();
    }

    /// Get the original language that Ghostty was launched with.
    pub fn saved_language(&self) -> Option<String> {
        self.imp().saved_language.borrow().clone()
    }

    /// Run the application. This is a replacement for `gio.Application.run`
    /// because we want more tight control over our event loop so we can
    /// integrate it with libghostty.
    pub fn run(&self) -> Result<(), RunError> {
        // Based on the actual `gio.Application.run` implementation:
        // https://github.com/GNOME/glib/blob/a8e8b742e7926e33eb635a8edceac74cf239d6ed/gio/gapplication.c#L2533

        // Acquire the default context for the application
        let ctx = glib::MainContext::default();
        let guard = ctx.acquire().map_err(|_| RunError::ContextAcquireFailed)?;

        let result = self.run_inner(&ctx);

        // The final cleanup that is always required at the end of running.
        {
            // Ensure our timer source is removed
            self.stop_quit_timer();

            // Sync any remaining settings
            gio::Settings::sync();

            // Clear out the event loop, don't block.
            while ctx.iteration(false) {}

            // Release the context so something else can use it.
            drop(guard);
        }

        result
    }

    fn run_inner(&self, ctx: &glib::MainContext) -> Result<(), RunError> {
        // Register the application
        if let Err(err) = self.register(None::<&gio::Cancellable>) {
            log::warn!(target: LOG, "error registering application: {}", err.message());
            return Err(RunError::ApplicationRegisterFailed);
        }

        // This just calls the `activate` signal but its part of the normal
        // startup routine so we just call it, but only if the config allows
        // it (this allows for launching Ghostty in the "background" without
        // immediately opening a window).
        //
        // https://gitlab.gnome.org/GNOME/glib/-/blob/bd2ccc2f69ecfd78ca3f34ab59e42e2b462bad65/gio/gapplication.c#L2302
        let priv_ = self.imp();
        {
            // We need to scope any config access because once we run our
            // event loop, this can change out from underneath us.
            let initial_window = self.get_config().map(|c| c.get().initial_window).unwrap_or(true);
            if initial_window {
                self.activate();
            }
        }

        // If we are NOT the primary instance, then we never want to run.
        // This means that another instance of the GTK app is running.
        if self.is_remote() {
            log::debug!(target: LOG, "application is remote, exiting run loop after activation");
            return Ok(());
        }

        // Tell systemd that we are ready.
        systemd::notify::ready();
        priv_.systemd_ready.set(true);

        log::debug!(target: LOG, "entering runloop");
        priv_.running.set(true);
        while priv_.running.get() {
            ctx.iteration(true);

            // Tick the core Ghostty terminal app
            // SAFETY: the runtime app outlives the run loop.
            let rt = unsafe { &*self.rt() };
            if let Err(err) = self.core().tick(rt) {
                log::debug!(target: LOG, "exiting runloop");
                return Err(RunError::Tick(err));
            }

            // Check if we must quit based on the current state.
            let must_quit = 'q: {
                // If we are configured to always stay running, don't quit.
                let Some(config) = self.get_config() else { break 'q false };
                let (quit_after, delay) = {
                    let c = config.get();
                    (c.quit_after_last_window_closed, c.quit_after_last_window_closed_delay)
                };
                if !quit_after {
                    break 'q false;
                }

                // If the quit timer has expired, quit.
                if matches!(*priv_.quit_timer.borrow(), QuitTimerState::Expired) {
                    log::debug!(target: LOG, "must_quit due to quit timer expired");
                    break 'q true;
                }

                // If we have no windows attached to our app, also quit.
                // We only do this if we don't have the closed delay set,
                // because with the closed delay set we'll exit eventually.
                if delay.is_none() && priv_.requested_window.get() && self.windows().is_empty() {
                    log::debug!(target: LOG, "must_quit due to no app windows");
                    break 'q true;
                }

                // No quit conditions met
                false
            };

            if must_quit {
                // All must quit scenarios do not need confirmation.
                // Furthermore, must quit scenarios may result in a situation
                // where its unsafe to even access the app/surface memory
                // since its in the process of being freed. We must simply
                // begin our exit immediately.
                self.quit_now();
            }
        }
        log::debug!(target: LOG, "exiting runloop");

        Ok(())
    }

    /// Quit the application. This will start the process to stop the
    /// run loop. It will not `posix.exit`.
    pub fn quit_app(&self) {
        let priv_ = self.imp();

        // If our run loop has already exited then we are done.
        if !priv_.running.get() {
            return;
        }

        // If our core app doesn't need to confirm quit then we
        // can exit immediately.
        if !self.core().needs_confirm_quit() {
            self.quit_now();
            return;
        }

        // Get the parent for our dialog: the active window, if any. If we
        // have an active surface then we should have a window available but
        // in the rare case we don't we should exit so we don't crash.
        let parent: Option<gtk4::Widget> = find_active_window().map(|w| w.upcast());

        // Show a confirmation dialog
        let dialog = CloseConfirmationDialog::new(close_confirmation_dialog::Target::App);
        let weak = self.downgrade();
        dialog.connect_close_request(move |_| {
            if let Some(this) = weak.upgrade() {
                this.quit_now();
            }
        });

        // Show it
        dialog.present(parent.as_ref());
    }

    fn quit_now(&self) {
        // Get all our windows and destroy them, forcing them to free.
        for window in gtk4::Window::list_toplevels() {
            // We only want to destroy our windows. These windows own
            // every other type of window that is possible so this will
            // trigger a proper shutdown sequence.
            //
            // We previously just destroyed ALL windows but this leads to
            // a double-free with the fcitx ime, because it has a nested
            // gtk.Window as a property that we don't own and it later
            // tries to free on its own. I think this is probably a bug in
            // the fcitx ime widget but still, we don't want a double free!
            if let Ok(window) = window.downcast::<Window>() {
                window.destroy();
            }
        }

        // Trigger our runloop exit.
        self.imp().running.set(false);
    }

    /// apprt API to perform an action.
    pub fn perform_action(&self, target: Target, action: RtAction<'_>) -> Result<bool, apprt::Error> {
        let err = |e: &dyn std::fmt::Display| apprt::Error(e.to_string());
        match action {
            RtAction::CloseTab(v) => return Ok(action_fns::close_tab(target, v)),
            RtAction::CloseWindow => return Ok(action_fns::close_window(target)),

            RtAction::CopyTitleToClipboard => return Ok(action_fns::copy_title_to_clipboard(target)),

            RtAction::ConfigChange(v) => action_fns::config_change(self, target, v.config),

            RtAction::DesktopNotification(v) => action_fns::desktop_notification(
                self,
                target,
                &v.title.to_string_lossy(),
                &v.body.to_string_lossy(),
            ),

            RtAction::EqualizeSplits => return Ok(action_fns::equalize_splits(target)),

            RtAction::GotoSplit(v) => return Ok(action_fns::goto_split(target, v)),

            RtAction::GotoWindow(v) => return Ok(action_fns::goto_window(v)),

            RtAction::GotoTab(v) => return Ok(action_fns::goto_tab(target, v)),

            RtAction::InitialSize(v) => return Ok(action_fns::initial_size(target, v)),

            RtAction::Inspector(v) => return Ok(action_fns::control_inspector(target, v)),
            RtAction::ExportTerminalIo(v) => return Ok(action_fns::export_terminal_io(target, v.contents)),

            RtAction::KeySequence(v) => return Ok(action_fns::key_sequence(target, &v)),
            RtAction::KeyTable(v) => return Ok(action_fns::key_table(target, &v)),

            RtAction::MouseOverLink(v) => action_fns::mouse_over_link(target, v),
            RtAction::MouseShape(v) => action_fns::mouse_shape(target, v),
            RtAction::MouseVisibility(v) => action_fns::mouse_visibility(target, v),

            RtAction::MoveTab(v) => return Ok(action_fns::move_tab(target, v)),
            RtAction::MoveTabToNewWindow => return Ok(action_fns::move_tab_to_new_window(target)),

            RtAction::NewSplit(v) => return Ok(action_fns::new_split(target, v)),

            RtAction::NewTab => return Ok(action_fns::new_tab(target, &Overrides::NONE)),

            RtAction::NewWindow => action_fns::new_window(
                self,
                match target {
                    Target::App => None,
                    Target::Surface(v) => Some(v),
                },
                &Overrides::NONE,
            ),

            RtAction::OpenConfig(v) => return Ok(action_fns::open_config(self, v)),

            RtAction::OpenUrl(v) => action_fns::open_url(self, v),

            RtAction::Pwd(v) => action_fns::pwd(target, &v.pwd.to_string_lossy()),

            RtAction::PresentTerminal => return Ok(action_fns::present_terminal(target)),

            RtAction::ProgressReport(v) => return Ok(action_fns::progress_report(target, v)),

            RtAction::PromptTitle(v) => return Ok(action_fns::prompt_title(target, v)),

            RtAction::Quit => self.quit_app(),

            RtAction::QuitTimer(v) => action_fns::quit_timer(self, v),

            RtAction::ReloadConfig(v) => action_fns::reload_config(self, target, v).map_err(|e| err(&e))?,

            RtAction::Render => action_fns::render(target),

            RtAction::ResizeSplit(v) => return Ok(action_fns::resize_split(target, v)),

            RtAction::ResizeWindow(v) => return Ok(action_fns::resize_window(target, v)),

            RtAction::RingBell => action_fns::ring_bell(target),

            // GTK has no accessibility consumer for this yet.
            RtAction::SelectionChanged => {}

            RtAction::Scrollbar(v) => action_fns::scrollbar(target, v),

            RtAction::SetTitle(v) => action_fns::set_title(target, v.title),
            RtAction::SetTabTitle(v) => return Ok(action_fns::set_tab_title(target, v.title)),
            RtAction::SetWindowTitle(v) => return Ok(action_fns::set_window_title(target, v.title)),

            RtAction::ShowChildExited(v) => return Ok(action_fns::show_child_exited(target, v)),

            RtAction::ShowGtkInspector => action_fns::show_gtk_inspector(),

            RtAction::SizeLimit(v) => return Ok(action_fns::size_limit(target, v)),

            RtAction::CellSize(v) => return Ok(action_fns::cell_size(target, v)),

            RtAction::ToggleMaximize => action_fns::toggle_maximize(target),
            RtAction::ToggleFullscreen(_) => action_fns::toggle_fullscreen(target),
            RtAction::ToggleQuickTerminal => return Ok(action_fns::toggle_quick_terminal(self)),
            RtAction::ToggleTabOverview => return Ok(action_fns::toggle_tab_overview(target)),
            RtAction::ToggleWindowDecorations => return Ok(action_fns::toggle_window_decorations(target)),
            RtAction::ToggleCommandPalette => return Ok(action_fns::toggle_command_palette(target)),
            RtAction::ToggleSplitZoom => return Ok(action_fns::toggle_split_zoom(target)),
            RtAction::ShowOnScreenKeyboard => return Ok(action_fns::show_on_screen_keyboard(target)),
            RtAction::CommandFinished(v) => return Ok(action_fns::command_finished(target, v)),
            RtAction::Readonly(v) => return Ok(action_fns::set_readonly(target, v)),

            RtAction::StartSearch(v) => action_fns::start_search(target, &v.needle.to_string_lossy()),
            RtAction::EndSearch => action_fns::end_search(target),
            RtAction::SearchTotal(v) => action_fns::search_total(target, v.total),
            RtAction::SearchSelected(v) => action_fns::search_selected(target, v.selected),

            // Unimplemented
            RtAction::SecureInput(_)
            | RtAction::CloseAllWindows
            | RtAction::FloatWindow(_)
            | RtAction::ToggleVisibility
            | RtAction::ToggleBackgroundOpacity
            | RtAction::RenderInspector
            | RtAction::RendererHealth(_)
            | RtAction::ColorChange(_)
            | RtAction::ResetWindowSize
            | RtAction::CheckForUpdates
            | RtAction::Undo
            | RtAction::Redo => {
                log::warn!(target: LOG, "unimplemented action={:?}", action.key());
                return Ok(false);
            }
        }

        // Assume it was handled. The unhandled case must be explicit
        // in the switch above.
        Ok(true)
    }

    /// Returns the core app associated with this application. This is
    /// not a reference-counted type so you should not store this.
    #[allow(clippy::mut_from_ref)]
    pub fn core(&self) -> &mut CoreApp {
        // SAFETY: the core app outlives the application and is only used
        // on the main thread.
        unsafe { &mut *self.imp().core_app.get() }
    }

    /// Returns the apprt application associated with this application.
    pub fn rt(&self) -> *mut apprt::App {
        self.imp().rt_app.get()
    }

    /// Runs `f` with the app winproto implementation (Zig: `winproto()`),
    /// or returns `None` after the application was deinitialized.
    pub fn with_winproto<R>(&self, f: impl FnOnce(&mut winproto::App) -> R) -> Option<R> {
        self.imp().winproto.borrow_mut().as_mut().map(f)
    }

    /// Runs `f` with the open URI portal implementation.
    pub fn with_open_uri<R>(&self, f: impl FnOnce(&OpenURI) -> R) -> Option<R> {
        self.imp().open_uri.borrow().as_ref().map(f)
    }

    /// The GObject surfaces of all the core app's surfaces.
    pub fn surfaces(&self) -> Vec<Surface> {
        let core = self.core();
        core.surfaces
            .iter()
            // SAFETY: the app's surface list only holds live surfaces.
            .map(|&s| crate::apprt::gtk::rt_surface(unsafe { &*s }).gobj())
            .collect()
    }

    /// This will get called when there are no more open surfaces.
    fn start_quit_timer(&self) {
        let priv_ = self.imp();
        let Some(config) = self.get_config() else { return };
        let (quit_after, delay) = {
            let c = config.get();
            (c.quit_after_last_window_closed, c.quit_after_last_window_closed_delay)
        };

        // Cancel any previous timer.
        self.stop_quit_timer();

        // This is a no-op unless we are configured to quit after last
        // window is closed.
        if !quit_after {
            return;
        }

        // If a delay is configured, set a timeout function to quit after
        // the delay.
        if let Some(v) = delay {
            let weak = self.downgrade();
            let id = glib::timeout_add_local_once(
                std::time::Duration::from_millis(v.as_milliseconds() as u64),
                move || {
                    if let Some(this) = weak.upgrade() {
                        this.imp().quit_timer.replace(QuitTimerState::Expired);
                    }
                },
            );
            priv_.quit_timer.replace(QuitTimerState::Active(id));
        } else {
            // If no delay is configured, treat it as expired.
            priv_.quit_timer.replace(QuitTimerState::Expired);
        }
    }

    /// This will get called when a new surface gets opened.
    fn stop_quit_timer(&self) {
        let priv_ = self.imp();
        let old = priv_.quit_timer.replace(QuitTimerState::Off);
        match old {
            QuitTimerState::Off | QuitTimerState::Expired => {}
            QuitTimerState::Active(source) => {
                // The source may already have fired (the timeout is
                // one-shot); removing it then would warn.
                let id = source.as_raw();
                // SAFETY: plain FFI lookup on the default context.
                let exists = unsafe {
                    !glib::ffi::g_main_context_find_source_by_id(std::ptr::null_mut(), id).is_null()
                };
                if exists {
                    source.remove();
                } else {
                    log::warn!(target: LOG, "unable to remove quit timer source={id}");
                }
            }
        }
    }

    fn load_runtime_css(&self) {
        let priv_ = self.imp();
        let Some(config_obj) = self.get_config() else { return };
        let contents = runtime_css(&config_obj.get());

        log::debug!(target: LOG, "runtime CSS is {} bytes", contents.len());

        // Clears any previously loaded CSS from this provider
        if let Some(p) = priv_.css_provider.borrow().as_ref() {
            p.load_from_bytes(&glib::Bytes::from_owned(contents.into_bytes()));
        }
    }

    fn load_custom_css(&self) {
        let priv_ = self.imp();
        let Some(display) = gdk::Display::default() else {
            log::warn!(target: LOG, "unable to get display");
            return;
        };

        // unload the previously loaded style providers
        for provider in priv_.custom_css_providers.borrow_mut().drain(..) {
            gtk4::style_context_remove_provider_for_display(&display, &provider);
        }

        let Some(config_obj) = self.get_config() else { return };
        let paths: Vec<ConfigPath> = config_obj.get().gtk_custom_css.value.clone();
        for p in paths {
            let (path, optional) = match &p {
                ConfigPath::Optional(path) => (path.as_str(), true),
                ConfigPath::Required(path) => (path.as_str(), false),
            };
            let file = match std::fs::File::open(path) {
                Ok(f) => f,
                Err(err) => {
                    if err.kind() != std::io::ErrorKind::NotFound || !optional {
                        log::warn!(target: LOG, "error opening gtk-custom-css file {path}: {err}");
                    }
                    continue;
                }
            };

            const CSS_FILE_SIZE_LIMIT: u64 = 5 * 1024 * 1024; // 5MB

            log::info!(target: LOG, "loading gtk-custom-css path={path}");

            use std::io::Read as _;
            let mut contents = Vec::new();
            match file.take(CSS_FILE_SIZE_LIMIT + 1).read_to_end(&mut contents) {
                Ok(n) if n as u64 > CSS_FILE_SIZE_LIMIT => {
                    log::warn!(target: LOG, "gtk-custom-css file {path} was larger than {CSS_FILE_SIZE_LIMIT}B");
                    continue;
                }
                Ok(_) => {}
                Err(err) => {
                    log::warn!(target: LOG, "failed to load custom CSS, no custom CSS applied, err={err}");
                    return;
                }
            }

            let css_provider = gtk4::CssProvider::new();
            css_provider.connect_parsing_error(|_, section, err| signal_css_parsing_error(section, err));

            priv_.custom_css_providers.borrow_mut().push(css_provider.clone());

            css_provider.load_from_bytes(&glib::Bytes::from_owned(contents));

            gtk4::style_context_add_provider_for_display(
                &display,
                &css_provider,
                gtk4::STYLE_PROVIDER_PRIORITY_USER,
            );
        }
    }

    fn sync_action_accelerators(&self) {
        use crate::input::binding::{CopyToClipboard, InspectorMode, OpenConfig, SplitDirection as BSplit};
        self.sync_action_accelerator("app.quit", BindingAction::Quit);
        self.sync_action_accelerator("app.open-config::os-open", BindingAction::OpenConfig(OpenConfig::OsOpen));
        self.sync_action_accelerator("app.open-config::new-window", BindingAction::OpenConfig(OpenConfig::NewWindow));
        self.sync_action_accelerator("app.reload-config", BindingAction::ReloadConfig);
        self.sync_action_accelerator("win.toggle-inspector", BindingAction::Inspector(InspectorMode::Toggle));
        self.sync_action_accelerator("app.show-gtk-inspector", BindingAction::ShowGtkInspector);
        self.sync_action_accelerator("win.toggle-command-palette", BindingAction::ToggleCommandPalette);
        self.sync_action_accelerator("win.close", BindingAction::CloseWindow);
        self.sync_action_accelerator("win.new-window", BindingAction::NewWindow);
        self.sync_action_accelerator("win.new-tab", BindingAction::NewTab);
        self.sync_action_accelerator(
            "win.close-tab::this",
            BindingAction::CloseTab(crate::input::binding::CloseTabMode::This),
        );
        self.sync_action_accelerator(
            "tab.close::this",
            BindingAction::CloseTab(crate::input::binding::CloseTabMode::This),
        );
        self.sync_action_accelerator("win.split-right", BindingAction::NewSplit(BSplit::Right));
        self.sync_action_accelerator("win.split-down", BindingAction::NewSplit(BSplit::Down));
        self.sync_action_accelerator("win.split-left", BindingAction::NewSplit(BSplit::Left));
        self.sync_action_accelerator("win.split-up", BindingAction::NewSplit(BSplit::Up));
        self.sync_action_accelerator("win.copy", BindingAction::CopyToClipboard(CopyToClipboard::Mixed));
        self.sync_action_accelerator("win.paste", BindingAction::PasteFromClipboard);
        self.sync_action_accelerator("win.reset", BindingAction::Reset);
        self.sync_action_accelerator("win.clear", BindingAction::ClearScreen);
        self.sync_action_accelerator("win.prompt-title", BindingAction::PromptSurfaceTitle);
        self.sync_action_accelerator("split-tree.new-split::left", BindingAction::NewSplit(BSplit::Left));
        self.sync_action_accelerator("split-tree.new-split::right", BindingAction::NewSplit(BSplit::Right));
        self.sync_action_accelerator("split-tree.new-split::up", BindingAction::NewSplit(BSplit::Up));
        self.sync_action_accelerator("split-tree.new-split::down", BindingAction::NewSplit(BSplit::Down));
    }

    fn sync_action_accelerator(&self, gtk_action: &str, action: BindingAction) {
        // Reset it initially
        self.set_accels_for_action(gtk_action, &[]);

        let Some(config) = self.get_config() else { return };
        let Some(trigger) = config.get().keybind.set.get_trigger(&action) else { return };
        let mut buf = [0u8; 1024];
        let accel = match key::accel_from_trigger(&mut buf, trigger) {
            Ok(Some(accel)) => accel.to_string_lossy().into_owned(),
            Ok(None) => return,
            // This should really never, never happen. Its not critical
            // enough to actually crash, but this is a bug somewhere. An
            // accelerator for a trigger can't possibly be more than 1024
            // bytes.
            Err(_) => {
                log::warn!(target: LOG, "accelerator somehow longer than 1024 bytes: {trigger:?}");
                return;
            }
        };

        self.set_accels_for_action(gtk_action, &[accel.as_str()]);
    }

    //---------------------------------------------------------------
    // Properties

    /// Returns the configuration for this application (a new reference).
    pub fn get_config(&self) -> Option<Config> {
        self.imp().config.borrow().clone()
    }

    /// Set the configuration for this application. The reference count
    /// is increased on the new configuration and the old one is
    /// unreferenced.
    ///
    /// If the config has errors this may show the config errors dialog.
    pub fn set_config(&self, config: &Config) {
        self.imp().config.replace(Some(config.clone()));
        self.notify(properties::CONFIG);

        // Show our errors if we have any
        self.show_config_errors_dialog();
    }

    fn prop_config(&self) {
        // Sync our accelerators for menu items.
        self.sync_action_accelerators();

        // Load our runtime and custom CSS. If this fails then our window is
        // just stuck with the old CSS but we don't want to fail the entire
        // config change operation.
        self.load_runtime_css();
        self.load_custom_css();
    }

    //---------------------------------------------------------------
    // Libghostty Callbacks

    /// Wake up the main loop (thread-safe).
    pub fn wakeup() {
        // SAFETY: waking the default main context is thread-safe.
        unsafe { glib::ffi::g_main_context_wakeup(std::ptr::null_mut()) };
    }

    //---------------------------------------------------------------
    // Virtual Methods

    fn startup(&self) {
        // Set ourselves as the default application.
        self.set_default();

        // The D-Bus connection is only valid after GApplication startup.
        let dbus = self.dbus_connection();
        self.with_open_uri(|o| o.set_dbus_connection(dbus.clone()));

        // Setup our event loop
        self.startup_xev();

        // Setup some signal handlers. This must happen before anything
        // that might notify systemd, since with Type=notify-reload systemd
        // refuses to start us if our reload signal has no handler.
        self.startup_signals();

        // Setup our style manager (light/dark mode)
        self.startup_style_manager();

        // Setup our action map
        self.startup_action_map();

        // Setup our global shortcuts
        self.startup_global_shortcuts();

        // If we have any config diagnostics from loading, then we
        // show the diagnostics dialog. We show this one as a general
        // modal (not to any specific window) because we don't even
        // know if the window will load.
        self.show_config_errors_dialog();
    }

    /// Configure libxev to use a specific backend.
    ///
    /// The Rust port has no libxev; the configured backend is only logged.
    fn startup_xev(&self) {
        let Some(config) = self.get_config() else { return };
        let backend = config.get().async_backend;

        // If our backend is auto then we have no setup to do.
        if backend == AsyncBackend::Auto {
            return;
        }

        log::warn!(target: LOG, "libxev manual backend failed, using default backend={backend:?}");
    }

    /// Setup the style manager on startup. The primary task here is to
    /// setup our initial light/dark mode based on the configuration and
    /// setup listeners for changes to the style manager.
    fn startup_style_manager(&self) {
        let Some(config) = self.get_config() else { return };

        // Setup our initial light/dark
        let style = self.style_manager();
        let scheme = {
            let config = config.get();
            match config.window_theme {
                WindowTheme::Auto | WindowTheme::Ghostty => {
                    let lum = config.background.to_terminal_rgb().perceived_luminance();
                    if lum > 0.5 {
                        libadwaita::ColorScheme::PreferLight
                    } else {
                        libadwaita::ColorScheme::PreferDark
                    }
                }
                WindowTheme::System => libadwaita::ColorScheme::PreferLight,
                WindowTheme::Dark => libadwaita::ColorScheme::ForceDark,
                WindowTheme::Light => libadwaita::ColorScheme::ForceLight,
            }
        };
        style.set_color_scheme(scheme);

        // Setup color change notifications
        let weak = self.downgrade();
        style.connect_notify_local(Some("dark"), move |style, _| {
            if let Some(this) = weak.upgrade() {
                this.handle_style_manager_dark(style);
            }
        });

        // Do an initial color scheme sync. This is idempotent and does
        // nothing if our current theme matches what libghostty has so its
        // safe to call.
        self.handle_style_manager_dark(&style);
    }

    /// Setup signal handlers
    fn startup_signals(&self) {
        let priv_ = self.imp();
        debug_assert!(priv_.signal_source.borrow().is_none());
        let weak = self.downgrade();
        let id = glib::unix_signal_add_local(libc::SIGUSR2, move || {
            if let Some(this) = weak.upgrade() {
                log::info!(target: LOG, "received SIGUSR2, reloading configuration");
                if let Err(err) = action_fns::reload_config(&this, Target::App, rt_action::ReloadConfig { soft: false }) {
                    // If we fail to reload the configuration, then we want
                    // the user to know it. For now we log but we should show
                    // another GUI.
                    log::warn!(target: LOG, "error reloading config: {err}");
                }
            }
            glib::ControlFlow::Continue
        });
        priv_.signal_source.replace(Some(id));
    }

    /// Setup our action map.
    fn startup_action_map(&self) {
        let as_ = glib::VariantTy::STRING_ARRAY;
        let tas = glib::VariantTy::new("(tas)").expect("valid variant type");
        let t = glib::VariantTy::UINT64;
        let s = glib::VariantTy::STRING;

        let actions = [
            GtkAction::init("new-window", Self::action_new_window, None),
            GtkAction::init("new-window-command", Self::action_new_window, Some(as_)),
            GtkAction::init("new-tab", Self::action_new_tab, Some(tas)),
            GtkAction::init("open-config", Self::action_open_config, Some(s)),
            GtkAction::init("present-surface", Self::action_present_surface, Some(t)),
            GtkAction::init("quit", Self::action_quit, None),
            GtkAction::init("reload-config", Self::action_reload_config, None),
            GtkAction::init("toggle-quick-terminal", Self::action_toggle_quick_terminal, None),
            GtkAction::init("ring-bell", Self::action_ring_bell, None),
        ];

        ext::actions::add(self, &actions);
    }

    /// Setup our global shortcuts.
    fn startup_global_shortcuts(&self) {
        let Some(global_shortcuts) = self.imp().global_shortcuts.borrow().clone() else { return };

        // On startup, our dbus connection should be available.
        global_shortcuts.set_dbus_connection(self.dbus_connection().as_ref());

        // Setup a binding so that the shortcut config always matches the
        // app.
        self.bind_property(properties::CONFIG, &global_shortcuts, "config")
            .sync_create()
            .build();

        // Setup the signal handler for global shortcut triggers
        let weak = self.downgrade();
        global_shortcuts.connect_trigger(move |_, action| {
            if let Some(this) = weak.upgrade() {
                // SAFETY: the runtime app outlives the application.
                let rt = unsafe { &*this.rt() };
                if let Err(err) = this.core().perform_all_action(rt, action.clone()) {
                    log::warn!(target: LOG, "failed to perform action={err}");
                }
            }
        });

        let weak = self.downgrade();
        global_shortcuts.connect_bind_failed(move |_, failure| {
            if let Some(this) = weak.upgrade() {
                this.global_shortcut_bind_failed(failure);
            }
        });
    }

    //---------------------------------------------------------------
    // Signal Handlers

    fn handle_style_manager_dark(&self, style: &libadwaita::StyleManager) {
        let scheme = if !style.is_dark() {
            apprt::ColorScheme::Light
        } else {
            apprt::ColorScheme::Dark
        };
        log::debug!(target: LOG, "style manager changed scheme={scheme:?}");

        let core_app = self.core();
        // SAFETY: the runtime app outlives the application.
        let rt = unsafe { &*self.rt() };
        if let Err(err) = core_app.color_scheme_event(rt, scheme) {
            log::warn!(target: LOG, "error updating app color scheme err={err}");
        }
        for &surface in core_app.surfaces.clone().iter() {
            // SAFETY: the app's surface list only holds live surfaces.
            let core = unsafe { (*surface).core() };
            // SAFETY: an initialized surface's core surface is live.
            if let Err(err) = unsafe { (*core).color_scheme_callback(scheme) } {
                log::warn!(target: LOG, "unable to tell surface about color scheme change err={err}");
            }
        }

        if gtk_version::at_least(4, 20, 0) {
            // `GtkInterfaceColorScheme`: 2 = dark, 3 = light.
            let value: i32 = match scheme {
                apprt::ColorScheme::Light => 3,
                apprt::ColorScheme::Dark => 2,
            };
            let set = |p: &gtk4::CssProvider| {
                if p.find_property("prefers-color-scheme").is_some() {
                    // SAFETY: the property is the GtkInterfaceColorScheme
                    // enum, set by value.
                    unsafe {
                        use glib::translate::ToGlibPtrMut;
                        let mut v = glib::Value::from_type(
                            p.find_property("prefers-color-scheme").unwrap().value_type(),
                        );
                        glib::gobject_ffi::g_value_set_enum(v.to_glib_none_mut().0, value);
                        p.set_property_from_value("prefers-color-scheme", &v);
                    }
                }
            };
            if let Some(p) = self.imp().css_provider.borrow().as_ref() {
                set(p);
            }
            for p in self.imp().custom_css_providers.borrow().iter() {
                set(p);
            }
        }
    }

    fn handle_reload_config(&self) {
        // We clear our dialog reference because its going to close
        // after response handling and we don't want to reuse it.
        self.imp().config_errors_dialog.set(None);

        // Reload our config as if the app reloaded.
        if let Err(err) = action_fns::reload_config(self, Target::App, rt_action::ReloadConfig { soft: false }) {
            // If we fail to reload the configuration, then we want the
            // user to know it. For now we log but we should show another
            // GUI.
            log::warn!(target: LOG, "error reloading config: {err}");
        }
    }

    /// Show the config errors dialog if the config on our application
    /// has diagnostics.
    fn show_config_errors_dialog(&self) {
        let priv_ = self.imp();
        let Some(config) = self.get_config() else { return };

        // If we already have a dialog, just update the config.
        if let Some(diag) = priv_.config_errors_dialog.upgrade() {
            diag.set_config(Some(&config));

            if !config.has_diagnostics() {
                diag.close();
            } else {
                diag.present(None);
            }

            return;
        }

        // No diagnostics, do nothing.
        if !config.has_diagnostics() {
            return;
        }

        // No dialog yet, initialize a new one. There's no need to unref
        // here because the widget that it becomes a part of takes
        // ownership.
        let dialog = ConfigErrorsDialog::new(&config);
        priv_.config_errors_dialog.set(Some(&dialog));

        // Connect to the reload signal so we know to reload our config.
        let weak = self.downgrade();
        dialog.connect_reload_config(move |_| {
            if let Some(this) = weak.upgrade() {
                this.handle_reload_config();
            }
        });

        // Show it
        dialog.present(None);
    }

    /// May fire before any window exists, hence a desktop notification
    /// rather than a toast.
    fn global_shortcut_bind_failed(&self, failure: &BindFailed) {
        let mut label = String::new();
        let label = match key::label_from_trigger(&mut label, failure.trigger) {
            Ok(true) => label,
            _ => "?".to_owned(),
        };

        let detail: &str = if !failure.message.is_empty() {
            &failure.message
        } else if failure.revoked {
            class::i18n_(c"The keybind was revoked by the system.")
        } else {
            class::i18n_(c"The keybind was denied by the system.")
        };

        let body = format!("{label}: {detail}");

        action_fns::desktop_notification(
            self,
            Target::App,
            class::i18n_(c"Global keybind unavailable"),
            &body,
        );
    }

    fn action_reload_config(_: &gio::SimpleAction, _: Option<&glib::Variant>, self_: &Self) {
        // SAFETY: the runtime app outlives the application.
        let rt = unsafe { &*self_.rt() };
        if let Err(err) = self_.core().perform_action(rt, ScopedApp::ReloadConfig) {
            log::warn!(target: LOG, "error reloading config err={err}");
        }
    }

    fn action_toggle_quick_terminal(_: &gio::SimpleAction, _: Option<&glib::Variant>, self_: &Self) {
        // SAFETY: the runtime app outlives the application.
        let rt = unsafe { &*self_.rt() };
        if let Err(err) = self_.core().perform_action(rt, ScopedApp::ToggleQuickTerminal) {
            log::warn!(target: LOG, "error toggling quick terminal err={err}");
        }
    }

    fn action_quit(_: &gio::SimpleAction, _: Option<&glib::Variant>, self_: &Self) {
        // SAFETY: the runtime app outlives the application.
        let rt = unsafe { &*self_.rt() };
        if let Err(err) = self_.core().perform_action(rt, ScopedApp::Quit) {
            log::warn!(target: LOG, "error quitting err={err}");
        }
    }

    /// Handle `app.new-window` and `app.new-window-command` GTK actions
    pub fn action_new_window(_: &gio::SimpleAction, parameter: Option<&glib::Variant>, self_: &Self) {
        log::debug!(target: LOG, "received new window action");

        let overrides = (|| {
            // were we given a parameter?
            let arguments = parameter?;

            // ensure that the supplied parameter is an array of strings
            if !arguments.is_type(glib::VariantTy::STRING_ARRAY) {
                log::warn!(
                    target: LOG,
                    "parameter is of type '{}', not '{}'",
                    arguments.type_(),
                    glib::VariantTy::STRING_ARRAY
                );
                return None;
            }

            Overrides::parse(arguments.iter()).ok()
        })();

        action_fns::new_window(self_, None, overrides.as_ref().unwrap_or(&Overrides::NONE));
    }

    /// Handle `app.new-tab` GTK action
    pub fn action_new_tab(_: &gio::SimpleAction, parameter: Option<&glib::Variant>, self_: &Self) {
        log::debug!(target: LOG, "received new tab action");

        let core_app = self_.core();

        // were we given a parameter?
        let Some(parameter) = parameter else {
            log::warn!(target: LOG, "app.new-tab did not receive a parameter");
            return;
        };

        log::warn!(target: LOG, "got parameter: {}", parameter.type_());

        let tas = glib::VariantTy::new("(tas)").expect("valid variant type");

        // ensure that the supplied parameter is an array of strings
        if !parameter.is_type(tas) {
            log::warn!(target: LOG, "parameter is of type '{}', not '{}'", parameter.type_(), tas);
            return;
        }

        let surface_id: u64 = parameter.child_value(0).get().unwrap_or(0);
        let arguments = parameter.child_value(1);
        let Ok(overrides) = Overrides::parse(arguments.iter()) else { return };
        let surface_id = if surface_id == 0 { None } else { Some(surface_id) };

        let surface: Option<*mut CoreSurface> = 'surface: {
            if let Some(surface_id) = surface_id {
                match core_app.find_surface_by_id(surface_id) {
                    Some(s) => break 'surface Some(s),
                    None => log::warn!(target: LOG, "new-tab: unable to find surface 0x{surface_id:0>16x}"),
                }
            }
            match core_app.focused_surface() {
                Some(s) => break 'surface Some(s),
                None => log::warn!(target: LOG, "new-tab: unable to find surface"),
            }
            None
        };

        if let Some(surface) = surface {
            if !action_fns::new_tab(Target::Surface(surface), &overrides) {
                log::warn!(target: LOG, "new-tab: unable to create tab");
            }
        } else {
            action_fns::new_window(self_, None, &overrides);
        }
    }

    pub fn action_open_config(_: &gio::SimpleAction, parameter: Option<&glib::Variant>, self_: &Self) {
        let target = 'target: {
            let Some(parameter) = parameter else {
                log::warn!(target: LOG, "no parameter provided to app.open-config action");
                break 'target core_app_mod::OpenConfig::OsOpen;
            };

            let Some(str) = parameter.str() else {
                log::warn!(
                    target: LOG,
                    "parameter to app.open-config action is of type '{}' not '{}'",
                    parameter.type_(),
                    glib::VariantTy::STRING
                );
                break 'target core_app_mod::OpenConfig::OsOpen;
            };

            match str {
                "os-open" => core_app_mod::OpenConfig::OsOpen,
                "new-window" => core_app_mod::OpenConfig::NewWindow,
                _ => {
                    log::warn!(target: LOG, "'{str}' is not configured as a target for the app.open-config action");
                    core_app_mod::OpenConfig::OsOpen
                }
            }
        };

        self_
            .core()
            .mailbox
            .push(core_app_mod::Message::OpenConfig(target), QueueTimeout::Forever);
    }

    fn action_present_surface(_: &gio::SimpleAction, parameter: Option<&glib::Variant>, self_: &Self) {
        let Some(parameter) = parameter else { return };

        // Make sure that we've received a u64 from the system.
        let Some(surface_id) = parameter.get::<u64>() else { return };

        // Convert the u64 to a core surface by using it as a surface ID.
        // A value of zero means that there was no target surface for the
        // notification so we don't focus any surface.
        if surface_id == 0 {
            return;
        }
        let Some(surface) = self_.core().find_surface_by_id(surface_id) else { return };

        self_.core().mailbox.push(
            core_app_mod::Message::SurfaceMessage {
                surface,
                message: apprt::surface::Message::PresentSurface,
            },
            QueueTimeout::Forever,
        );
    }

    pub fn action_ring_bell(_: &gio::SimpleAction, _: Option<&glib::Variant>, self_: &Self) {
        let priv_ = self_.imp();
        let Some(config) = self_.get_config() else { return };
        let (audio, path, volume) = {
            let c = config.get();
            (c.bell_features.audio, c.bell_audio_path.clone(), c.bell_audio_volume)
        };

        // Do our sound
        if audio {
            let Some(config_path) = path else { return };
            let (path, required) = match &config_path {
                ConfigPath::Optional(path) => (path.as_str(), false),
                ConfigPath::Required(path) => (path.as_str(), true),
            };

            let volume = volume.clamp(0.0, 1.0);

            // Reuse one MediaFile per application (rebuilt only when the
            // path changes) so each bell replays the same pipeline instead
            // of leaking a fresh one. Assign unconditionally: bellMediaFile
            // frees any stale MediaFile and returns the current slot value
            // (possibly null if the path is now inaccessible), so
            // priv.bell_media never dangles.
            let current = priv_.bell_media.take();
            let media_file = media::bell_media_file(current, path, required);
            priv_.bell_media.replace(media_file.clone());
            let Some(media_file) = media_file else { return };
            media::play_bell(&media_file, volume);
        }
    }

    pub fn open_url_fallback(&self, kind: OpenUrlKind, url: &str) {
        // Fallback to the minimal cross-platform way of opening a URL.
        // This is always a safe fallback and enables for example Windows
        // to open URLs (GTK on Windows via WSL is a thing).
        if let Err(err) = internal_os::open(kind, url) {
            log::warn!(target: LOG, "unable to open url: {err:?}");
        }
    }
}

/// Log CSS parsing error
fn signal_css_parsing_error(css_section: &gtk4::CssSection, err: &glib::Error) {
    // Zig also logs the CSS source from `gtk_css_section_get_bytes`, which
    // needs GTK 4.16 (newer than the gtk-rs bindings the port targets).
    let location = css_section.to_str();
    log::warn!(target: LOG, "css parsing failed at {}: {} {}", location, err.domain().as_str(), err.message());
}

/// The runtime CSS for the given config (Zig: the body of
/// `loadRuntimeCss`).
pub fn runtime_css(config: &CoreConfig) -> String {
    let mut buf = String::with_capacity(2048);

    let window_theme = config.window_theme;
    let headerbar_background: ConfigColor = config.window_titlebar_background.unwrap_or(config.background);
    let headerbar_foreground: ConfigColor = config.window_titlebar_foreground.unwrap_or(config.foreground);

    buf.push_str(
        r#"/*
 * Child Exited Overlay
 */

.child-exited.normal revealer widget {
  background-color: color-mix(
    in srgb,
    var(--success-bg-color),
    transparent 50%
  );
}

.child-exited.abnormal revealer widget {
  background-color: color-mix(
    in srgb,
    var(--error-bg-color),
    transparent 50%
  );
}

/*
 * Surface
 */

.surface progressbar.error trough progress {
  background-color: color-mix(
    in srgb,
    var(--error-bg-color),
    transparent 50%
  );
}

.surface .bell-overlay {
  border-color: color-mix(
    in srgb,
    var(--accent-color),
    transparent 50%
  );
}

/*
 * Drag and Drop Overlay
 */
.drop-overlay.drop-left {
  background: linear-gradient(
    to left,
    transparent, 50%,
    color-mix(in srgb, var(--accent-bg-color), transparent 80%) 50%
  );
}
.drop-overlay.drop-right {
  background: linear-gradient(
    to right,
    transparent, 50%,
    color-mix(in srgb, var(--accent-bg-color), transparent 80%) 50%
  );
}
.drop-overlay.drop-top {
  background: linear-gradient(
    to top,
    transparent, 50%,
    color-mix(in srgb, var(--accent-bg-color), transparent 80%) 50%
  );
}
.drop-overlay.drop-bottom {
  background: linear-gradient(
    to bottom,
    transparent, 50%,
    color-mix(in srgb, var(--accent-bg-color), transparent 80%) 50%
  );
}

/*
 * Splits
 */

.window .split paned > separator {
  background-color: color-mix(
    in srgb,
    var(--window-bg-color),
    transparent 0%
  );
}
"#,
    );

    if window_theme == WindowTheme::Ghostty {
        let _ = write!(
            buf,
            r#":root {{
  --ghostty-fg: rgb({},{},{});
  --ghostty-bg: rgb({},{},{});
  --headerbar-fg-color: var(--ghostty-fg);
  --headerbar-bg-color: var(--ghostty-bg);
  --headerbar-backdrop-color: oklab(from var(--headerbar-bg-color) calc(l * 0.9) a b / alpha);
  --overview-fg-color: var(--ghostty-fg);
  --overview-bg-color: var(--ghostty-bg);
  --popover-fg-color: var(--ghostty-fg);
  --popover-bg-color: var(--ghostty-bg);
  --window-fg-color: var(--ghostty-fg);
  --window-bg-color: var(--ghostty-bg);
}}
listview.command-palette-view {{
  --view-fg-color: var(--ghostty-fg);
  --view-bg-color: var(--ghostty-bg);
}}
windowhandle {{
  background-color: var(--headerbar-bg-color);
  color: var(--headerbar-fg-color);
}}
windowhandle:backdrop {{
 background-color: var(--headerbar-backdrop-color);
}}"#,
            headerbar_foreground.r,
            headerbar_foreground.g,
            headerbar_foreground.b,
            headerbar_background.r,
            headerbar_background.g,
            headerbar_background.b,
        );
    }

    let unfocused_fill = config.unfocused_split_fill.unwrap_or(config.background);

    let _ = write!(
        buf,
        "widget.unfocused-split {{\n opacity: {:.2};\n background-color: rgb({},{},{});\n}}\n",
        1.0 - config.unfocused_split_opacity,
        unfocused_fill.r,
        unfocused_fill.g,
        unfocused_fill.b,
    );

    if let Some(color) = config.split_divider_color {
        let _ = write!(
            buf,
            ".window .split paned > separator {{\n  color: rgb({r},{g},{b});\n  background: rgb({r},{g},{b});\n}}\n",
            r = color.r,
            g = color.g,
            b = color.b,
        );
    }

    if let Some(font_family) = &config.window_title_font_family {
        let _ = write!(
            buf,
            ".window headerbar {{\n  font-family: \"{}\";\n}}\n",
            font_family.as_str()
        );
    }

    buf
}

/// The `GDK_DEBUG` / `GDK_DISABLE` values for [`set_gtk_env`].
fn gtk_env_values(stderr_logging: bool, gtk_opengl_debug: bool) -> (String, String) {
    // Output OpenGL debug information, `gtk-opengl-debug` dumps logs
    // directly to stderr so both must be true to enable OpenGL debugging.
    let gdk_debug_opengl = stderr_logging && gtk_opengl_debug;

    // Even though we don't use GTK's GL context anymore, there can still
    // occasionally be conflicts when Vulkan and OpenGL are used together.
    // Disabling Vulkan also saves hundreds of milliseconds of
    // initialization time on certain systems.
    let gdk_disable_vulkan = true;

    let mut debug = Vec::new();
    if gdk_debug_opengl {
        debug.push("opengl");
    }
    let mut disable = Vec::new();
    if gdk_disable_vulkan {
        disable.push("vulkan");
    }
    (debug.join(","), disable.join(","))
}

/// This sets various GTK-related environment variables as necessary
/// given the runtime environment or configuration.
///
/// This must be called BEFORE GTK initialization.
fn set_gtk_env(config: &CoreConfig) {
    debug_assert!(!gtk4::is_initialized());

    let (debug, disable) = gtk_env_values(global::logging().stderr, config.gtk_opengl_debug);

    log::warn!(target: LOG, "setting GDK_DEBUG={debug}");
    std::env::set_var("GDK_DEBUG", &debug);

    log::warn!(target: LOG, "setting GDK_DISABLE={disable}");
    std::env::set_var("GDK_DISABLE", &disable);

    // Sync environ after altering system env
    global::sync_environ();
}

/// The active toplevel window, if any (Zig: `findActiveWindow`).
fn find_active_window() -> Option<gtk4::Window> {
    gtk4::Window::list_toplevels()
        .into_iter()
        .filter_map(|w| w.downcast::<gtk4::Window>().ok())
        .find(|w| w.is_active())
}

/// All apprt action handlers
pub(crate) mod action_fns {
    use super::*;
    use crate::apprt::action::{
        CellSize, CommandFinished, Inspector, InitialSize, KeySequence, KeyTable, MouseOverLink, MouseVisibility,
        MoveTab, OpenConfig, PromptTitle, Readonly, ReloadConfig, ResizeSplit, ResizeSplitDirection, ResizeWindow,
        SizeLimit,
    };
    use crate::terminal::osc::ProgressReport;
    use crate::terminal::page_list::Scrollbar;

    /// The GObject surface of a surface target.
    fn gobj(core: *mut CoreSurface) -> Surface {
        // SAFETY: action targets are live GTK core surfaces.
        unsafe { crate::apprt::gtk::gobj_of(core) }
    }

    pub fn close_tab(target: Target, value: CloseTabMode) -> bool {
        match target {
            Target::App => false,
            Target::Surface(core) => {
                let surface = gobj(core);
                let mode = match value {
                    CloseTabMode::This => "this",
                    CloseTabMode::Other => "other",
                    CloseTabMode::Right => "right",
                };
                surface.activate_action("tab.close", Some(&mode.to_variant())).is_ok()
            }
        }
    }

    pub fn close_window(target: Target) -> bool {
        match target {
            Target::App => false,
            Target::Surface(core) => gobj(core).activate_action("win.close", None).is_ok(),
        }
    }

    pub fn copy_title_to_clipboard(target: Target) -> bool {
        match target {
            Target::App => false,
            Target::Surface(v) => gobj(v).copy_title_to_clipboard(),
        }
    }

    #[allow(deprecated)] // Zig uses the same (deprecated since 4.10) FileChooserNative.
    pub fn export_terminal_io(target: Target, contents: &[u8]) -> bool {
        let surface = match target {
            Target::App => return false,
            Target::Surface(v) => gobj(v),
        };

        let contents = contents.to_vec();

        let parent = ext::get_ancestor::<gtk4::Window>(&surface);
        let dialog = gtk4::FileChooserNative::new(
            Some(class::i18n_(c"Export Terminal IO Events")),
            parent.as_ref(),
            gtk4::FileChooserAction::Save,
            Some(class::i18n_(c"Export")),
            Some(class::i18n_(c"Cancel")),
        );
        dialog.set_create_folders(true);
        dialog.set_current_name("ghostty-terminal-io.txt");

        dialog.connect_response(move |dialog, response_id| {
            if response_id != gtk4::ResponseType::Accept {
                return;
            }

            let Some(file) = dialog.file() else {
                log::warn!(target: LOG, "inspector export dialog returned no file");
                return;
            };

            if let Err(err) = file.replace_contents(
                &contents,
                None,
                false,
                gio::FileCreateFlags::NONE,
                None::<&gio::Cancellable>,
            ) {
                log::error!(target: LOG, "failed to export terminal IO events err={}", err.message());
            }
        });
        dialog.show();
        // The native dialog keeps itself alive until it responds (Zig
        // unrefs it in the response handler).
        std::mem::forget(dialog);
        true
    }

    pub fn config_change(self_: &Application, target: Target, new_config: &CoreConfig) {
        // Wrap our config in a GObject. This will clone it.
        let config_obj = Config::new(new_config);

        match target {
            Target::Surface(core) => gobj(core).set_config(&config_obj),
            Target::App => self_.set_config(&config_obj),
        }
    }

    pub fn desktop_notification(self_: &Application, target: Target, title: &str, body: &str) {
        match target {
            Target::App => {}
            Target::Surface(v) => {
                gobj(v).send_desktop_notification(title, body);
                return;
            }
        }

        // Set a default title if we don't already have one
        let t = if title.is_empty() { "Ghostty" } else { title };

        let notification = gio::Notification::new(t);
        notification.set_body(Some(body));

        let icon = gio::ThemedIcon::new("com.mitchellh.ghostty");
        notification.set_icon(&icon);
        notification.set_default_action_and_target_value("app.present-surface", Some(&0u64.to_variant()));

        // We set the notification ID to the body content. If the content is
        // the same, this notification may replace a previous notification
        self_.send_notification(Some(body), &notification);
    }

    pub fn equalize_splits(target: Target) -> bool {
        match target {
            Target::App => {
                log::warn!(target: LOG, "equalize splits to app is unexpected");
                false
            }
            Target::Surface(core) => gobj(core).activate_action("split-tree.equalize", None).is_ok(),
        }
    }

    pub fn goto_split(target: Target, to: GotoSplit) -> bool {
        match target {
            Target::App => false,
            Target::Surface(core) => {
                // Design note: we can't use widget actions here because
                // we need to know whether there is a goto target for
                // returning the proper perform result (boolean).
                let surface = gobj(core);
                let Some(tree) = ext::get_ancestor::<SplitTree>(&surface) else {
                    log::warn!(target: LOG, "surface is not in a split tree, ignoring goto_split");
                    return false;
                };

                tree.goto(match to {
                    GotoSplit::Previous => Goto::PreviousWrapped,
                    GotoSplit::Next => Goto::NextWrapped,
                    GotoSplit::Up => Goto::Spatial(SpatialDirection::Up),
                    GotoSplit::Down => Goto::Spatial(SpatialDirection::Down),
                    GotoSplit::Left => Goto::Spatial(SpatialDirection::Left),
                    GotoSplit::Right => Goto::Spatial(SpatialDirection::Right),
                })
            }
        }
    }

    pub fn goto_tab(target: Target, tab: GotoTab) -> bool {
        match target {
            Target::App => false,
            Target::Surface(core) => {
                let surface = gobj(core);
                let Some(window) = ext::get_ancestor::<Window>(&surface) else {
                    log::warn!(target: LOG, "surface is not in a window, ignoring new_tab");
                    return false;
                };

                window.select_tab(match tab {
                    GotoTab::PREVIOUS => SelectTab::Previous,
                    GotoTab::NEXT => SelectTab::Next,
                    GotoTab::LAST => SelectTab::Last,
                    GotoTab(n) => SelectTab::N(n.max(0) as usize),
                })
            }
        }
    }

    pub fn goto_window(direction: GotoWindow) -> bool {
        let list: Vec<gtk4::Window> = gtk4::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk4::Window>().ok())
            .collect();
        if list.is_empty() {
            return false;
        }

        // The window we're starting from is typically our active window.
        let starting = list.iter().position(|w| w.is_active()).unwrap_or(0);

        // Go forward or backwards in the list until we find a valid
        // window that is visible.
        let order: Vec<usize> = match direction {
            GotoWindow::Next => (starting..list.len()).collect(),
            GotoWindow::Previous => (0..=starting).rev().collect(),
        };
        for i in order {
            if goto_window_maybe(&list[i]) {
                return true;
            }
        }

        // If we reached here, we didn't find a valid window to focus.
        // Wrap around.
        let wrap: Vec<usize> = match direction {
            GotoWindow::Next => (0..list.len()).collect(),
            GotoWindow::Previous => (0..list.len()).rev().collect(),
        };
        for i in wrap {
            if i == starting {
                break;
            }
            if goto_window_maybe(&list[i]) {
                return true;
            }
        }

        false
    }

    fn goto_window_maybe(gtk_window: &gtk4::Window) -> bool {
        // If it is already active skip it.
        if gtk_window.is_active() {
            return false;
        }
        // If it is hidden, skip it.
        if !gtk_window.is_visible() {
            return false;
        }
        // If it isn't a Ghostty window, skip it.
        let Some(window) = gtk_window.downcast_ref::<Window>() else { return false };

        // Focus our active surface
        let Some(surface) = window.get_active_surface() else { return false };
        gtk_window.present();
        surface.grab_focus();
        true
    }

    pub fn initial_size(target: Target, value: InitialSize) -> bool {
        match target {
            Target::App => false,
            Target::Surface(core) => {
                gobj(core).set_default_size(Size {
                    width: value.width,
                    height: value.height,
                });
                true
            }
        }
    }

    pub fn mouse_over_link(target: Target, value: MouseOverLink<'_>) {
        match target {
            Target::App => log::warn!(target: LOG, "mouse over link to app is unexpected"),
            Target::Surface(surface) => {
                let url = value.url.to_string_lossy();
                gobj(surface).set_mouse_hover_url(if !url.is_empty() { Some(&url) } else { None });
            }
        }
    }

    pub fn mouse_shape(target: Target, shape: crate::terminal::mouse::Shape) {
        match target {
            Target::App => log::warn!(target: LOG, "mouse shape to app is unexpected"),
            Target::Surface(surface) => gobj(surface).set_mouse_shape(shape.into()),
        }
    }

    pub fn mouse_visibility(target: Target, visibility: MouseVisibility) {
        match target {
            Target::App => log::warn!(target: LOG, "mouse visibility to app is unexpected"),
            Target::Surface(surface) => gobj(surface).set_mouse_hidden(match visibility {
                MouseVisibility::Visible => false,
                MouseVisibility::Hidden => true,
            }),
        }
    }

    pub fn move_tab(target: Target, value: MoveTab) -> bool {
        match target {
            Target::App => false,
            Target::Surface(core) => {
                let surface = gobj(core);
                let Some(window) = ext::get_ancestor::<Window>(&surface) else {
                    log::warn!(target: LOG, "surface is not in a window, ignoring new_tab");
                    return false;
                };

                window.move_tab(&surface, value.amount)
            }
        }
    }

    pub fn move_tab_to_new_window(target: Target) -> bool {
        match target {
            Target::App => false,
            Target::Surface(core) => {
                let surface = gobj(core);
                let Some(window) = ext::get_ancestor::<Window>(&surface) else {
                    log::warn!(target: LOG, "surface is not in a window, ignoring new_tab_to_new_window");
                    return false;
                };

                window.move_tab_to_new_window(&surface)
            }
        }
    }

    pub fn new_split(target: Target, direction: SplitDirection) -> bool {
        match target {
            Target::App => {
                log::warn!(target: LOG, "new split to app is unexpected");
                false
            }

            Target::Surface(core) => {
                let dir = match direction {
                    SplitDirection::Right => "right",
                    SplitDirection::Down => "down",
                    SplitDirection::Left => "left",
                    SplitDirection::Up => "up",
                };
                gobj(core)
                    .activate_action("split-tree.new-split", Some(&dir.to_variant()))
                    .is_ok()
            }
        }
    }

    pub fn new_tab(target: Target, overrides: &Overrides) -> bool {
        match target {
            Target::App => {
                log::warn!(target: LOG, "new tab to app is unexpected");
                false
            }

            Target::Surface(core) => {
                // Get the window ancestor of the surface. Surfaces shouldn't
                // be aware they might be in windows but at the app level we
                // can do this.
                let surface = gobj(core);
                let Some(window) = ext::get_ancestor::<Window>(&surface) else {
                    log::warn!(target: LOG, "surface is not in a window, ignoring new_tab");
                    return false;
                };
                window.new_tab(Some(core), overrides);
                true
            }
        }
    }

    pub fn new_window(self_: &Application, parent: Option<*mut CoreSurface>, overrides: &Overrides) {
        // Note that we've requested a window at least once. This is used
        // to trigger quit on no windows. Note I'm not sure if this is
        // REALLY necessary, but I don't want to risk a bug where on a slow
        // machine or something we quit immediately after starting up
        // because there was a delay in the event loop before we created a
        // Window.
        self_.imp().requested_window.set(true);

        let win = Window::new(self_, overrides.title.as_deref());
        init_and_show_window(self_, &win, parent, overrides);
    }

    pub fn init_and_show_window(
        self_: &Application,
        win: &Window,
        parent: Option<*mut CoreSurface>,
        overrides: &Overrides,
    ) {
        // Setup a binding so that whenever our config changes so does the
        // window. There's never a time when the window config should be out
        // of sync with the application config.
        self_.bind_property(properties::CONFIG, win, "config").build();

        // Create a new tab with window context (first tab in new window)
        win.new_tab_for_window(parent, overrides);

        // Estimate the initial window size before presenting so the window
        // manager can position it correctly.
        if let Some(surface) = win.get_active_surface() {
            surface.estimate_initial_size();
            if let Some(size) = surface.get_default_size() {
                win.set_default_size(size.width as i32, size.height as i32);
            }
        }

        // Show the window
        win.present();
    }

    pub fn open_config(self_: &Application, value: OpenConfig) -> bool {
        // Get the config file path
        let path = match crate::config::edit::open_path() {
            Ok(p) => p,
            Err(err) => {
                log::warn!(target: LOG, "error getting config file path: {err}");
                return false;
            }
        };

        match value {
            OpenConfig::OsOpen => {
                // Open it using openUrl. "path" isn't actually a URL but
                // at the time of writing that works just fine for GTK.
                open_url(
                    self_,
                    OpenUrl {
                        kind: OpenUrlKind::Text,
                        url: path.as_bytes(),
                    },
                );
            }

            OpenConfig::NewWindow => {
                let cmd = match internal_os::get_config_edit_command(path.as_str(), Default::default()) {
                    Ok(c) => c,
                    Err(err) => {
                        log::warn!(target: LOG, "unable to get command to edit the config: {err:?}");
                        return false;
                    }
                };

                let command = Command::Direct(vec!["/bin/sh".into(), "-c".into(), cmd.into()]);

                let title = format!("{} {}", class::i18n_(c"Editing configuration file"), path.as_str());

                new_window(
                    self_,
                    None,
                    &Overrides {
                        command: Some(command),
                        title: Some(title),
                        shell_integration: None,
                        working_directory: None,
                    },
                );
            }
        }

        true
    }

    pub fn open_url(self_: &Application, value: OpenUrl<'_>) {
        let url = String::from_utf8_lossy(value.url).into_owned();
        if url.starts_with('/') {
            self_.open_url_fallback(value.kind, &url);
            return;
        }
        if url.starts_with("file://") {
            self_.open_url_fallback(value.kind, &url);
            return;
        }

        let result = self_.with_open_uri(|o| o.start(value));
        if let Some(Err(err)) = result {
            log::error!(target: LOG, "unable to open uri err={err:?}");
            self_.open_url_fallback(value.kind, &url);
        }
    }

    pub fn pwd(target: Target, value: &str) {
        match target {
            Target::App => log::warn!(target: LOG, "pwd to app is unexpected"),
            Target::Surface(surface) => gobj(surface).set_pwd(Some(value)),
        }
    }

    pub fn quit_timer(self_: &Application, mode: QuitTimer) {
        match mode {
            QuitTimer::Start => self_.start_quit_timer(),
            QuitTimer::Stop => self_.stop_quit_timer(),
        }
    }

    pub fn present_terminal(target: Target) -> bool {
        match target {
            Target::App => false,
            Target::Surface(v) => {
                gobj(v).present();
                true
            }
        }
    }

    pub fn progress_report(target: Target, value: ProgressReport) -> bool {
        match target {
            Target::App => false,
            Target::Surface(v) => {
                gobj(v).set_progress_report(value);
                true
            }
        }
    }

    pub fn prompt_title(target: Target, value: PromptTitle) -> bool {
        let Target::Surface(v) = target else { return false };
        let surface = gobj(v);
        match value {
            PromptTitle::Surface => {
                surface.prompt_title();
                true
            }
            PromptTitle::Tab => {
                let Some(tab) = ext::get_ancestor::<Tab>(&surface) else {
                    log::warn!(target: LOG, "surface is not in a tab, ignoring prompt_tab_title");
                    return false;
                };
                tab.prompt_tab_title();
                true
            }
            PromptTitle::Window => {
                let Some(win) = ext::get_ancestor::<Window>(&surface) else {
                    log::warn!(target: LOG, "surface is not in a window, ignoring prompt_window_title");
                    return false;
                };
                win.prompt_window_title();
                true
            }
        }
    }

    /// Reload the configuration for the application and propagate it
    /// across the entire application and all terminals.
    pub fn reload_config(self_: &Application, target: Target, opts: ReloadConfig) -> Result<(), String> {
        // Tell systemd that reloading has started, but only once startup
        // is complete. A reload during startup is not a reload to systemd.
        let notify_systemd = self_.imp().systemd_ready.get();
        if notify_systemd {
            systemd::notify::reloading();
        }

        let result = (|| {
            // Get our config object.
            let config: Config = if opts.soft {
                // Soft-reloading applies conditional logic to the existing
                // loaded config so we return that as-is (but take a
                // reference).
                self_.get_config().ok_or("no config")?
            } else {
                // Hard reload, load a new config completely.
                let config = CoreConfig::load().map_err(|e| e.to_string())?;
                Config::new(&config)
            };

            // Update the proper target. This will trigger a
            // `config_change` apprt action which will propagate the config
            // properly to our property system.
            let snapshot = config.get().clone();
            match target {
                Target::App => {
                    // SAFETY: the runtime app outlives the application.
                    let rt = unsafe { &*self_.rt() };
                    self_.core().update_config(rt, &snapshot).map_err(|e| e.to_string())?
                }
                // SAFETY: action targets are live core surfaces.
                Target::Surface(core) => unsafe { (*core).update_config(&snapshot) }.map_err(|e| e.to_string())?,
            }
            Ok(())
        })();

        // When we exit this function tell systemd that reloading has
        // finished.
        if notify_systemd {
            systemd::notify::ready();
        }

        result
    }

    pub fn render(target: Target) {
        match target {
            Target::App => {}
            Target::Surface(v) => gobj(v).redraw(),
        }
    }

    pub fn resize_split(target: Target, value: ResizeSplit) -> bool {
        match target {
            Target::App => {
                log::warn!(target: LOG, "resize_split to app is unexpected");
                false
            }
            Target::Surface(core) => {
                let surface = gobj(core);
                let Some(tree) = ext::get_ancestor::<SplitTree>(&surface) else {
                    log::warn!(target: LOG, "surface is not in a split tree, ignoring resize_split");
                    return false;
                };

                // If the tree has no splits (only one leaf), this action is
                // not performable. This allows the key event to pass through
                // to the terminal.
                if !tree.get_is_split() {
                    return false;
                }

                match tree.resize(
                    match value.direction {
                        ResizeSplitDirection::Up => Direction::Up,
                        ResizeSplitDirection::Down => Direction::Down,
                        ResizeSplitDirection::Left => Direction::Left,
                        ResizeSplitDirection::Right => Direction::Right,
                    },
                    value.amount,
                ) {
                    Ok(v) => v,
                    Err(_) => {
                        log::warn!(target: LOG, "unable to resize split, out of memory");
                        false
                    }
                }
            }
        }
    }

    pub fn resize_window(target: Target, value: ResizeWindow) -> bool {
        match target {
            Target::App => {
                log::warn!(target: LOG, "resize_window to app is unexpected");
                false
            }
            Target::Surface(core) => {
                let surface = gobj(core);
                let Some(window) = ext::get_ancestor::<Window>(&surface) else {
                    log::warn!(target: LOG, "surface is not in a window, ignoring resize_window");
                    return false;
                };

                window.resize_surface(&surface, value)
            }
        }
    }

    pub fn ring_bell(target: Target) {
        match target {
            Target::App => {}
            Target::Surface(v) => gobj(v).set_bell_ringing(true),
        }
    }

    pub fn scrollbar(target: Target, value: Scrollbar) {
        match target {
            Target::App => {}
            Target::Surface(v) => gobj(v).set_scrollbar(value),
        }
    }

    pub fn start_search(target: Target, needle: &str) {
        match target {
            Target::App => {}
            Target::Surface(v) => gobj(v).set_search_active(true, needle),
        }
    }

    pub fn end_search(target: Target) {
        match target {
            Target::App => {}
            Target::Surface(v) => gobj(v).set_search_active(false, ""),
        }
    }

    pub fn search_total(target: Target, total: Option<usize>) {
        match target {
            Target::App => {}
            Target::Surface(v) => gobj(v).set_search_total(total),
        }
    }

    pub fn search_selected(target: Target, selected: Option<usize>) {
        match target {
            Target::App => {}
            Target::Surface(v) => gobj(v).set_search_selected(selected),
        }
    }

    pub fn set_title(target: Target, title: &CStr) {
        match target {
            Target::App => log::warn!(target: LOG, "set_title to app is unexpected"),
            Target::Surface(surface) => gobj(surface).set_title(Some(title)),
        }
    }

    pub fn set_tab_title(target: Target, title: &CStr) -> bool {
        match target {
            Target::App => {
                log::warn!(target: LOG, "set_tab_title to app is unexpected");
                false
            }
            Target::Surface(core) => {
                let surface = gobj(core);
                let Some(tab) = ext::get_ancestor::<Tab>(&surface) else {
                    log::warn!(target: LOG, "surface is not in a tab, ignoring set_tab_title");
                    return false;
                };
                let title = title.to_string_lossy();
                tab.set_title_override(if title.is_empty() { None } else { Some(&title) });
                true
            }
        }
    }

    pub fn set_window_title(target: Target, title: &CStr) -> bool {
        match target {
            Target::App => {
                log::warn!(target: LOG, "set_tab_title to app is unexpected");
                false
            }
            Target::Surface(core) => {
                let surface = gobj(core);
                let Some(window) = ext::get_ancestor::<Window>(&surface) else {
                    log::warn!(target: LOG, "surface is not in a window, ignoring set_window_title");
                    return false;
                };
                let title = title.to_string_lossy();
                window.set_title_override(if title.is_empty() { None } else { Some(&title) });
                true
            }
        }
    }

    pub fn show_child_exited(target: Target, value: ChildExited) -> bool {
        match target {
            Target::App => false,
            Target::Surface(v) => gobj(v).child_exited(value),
        }
    }

    pub fn show_gtk_inspector() {
        gtk4::Window::set_interactive_debugging(true);
    }

    pub fn size_limit(target: Target, value: SizeLimit) -> bool {
        match target {
            Target::App => false,
            Target::Surface(core) => {
                // Note: we ignore the max size currently because we have
                // no mechanism to enforce it.
                gobj(core).set_min_size(Size {
                    width: value.min_width,
                    height: value.min_height,
                });

                true
            }
        }
    }

    pub fn cell_size(target: Target, value: CellSize) -> bool {
        match target {
            Target::App => false,
            Target::Surface(core) => {
                gobj(core).set_cell_size(value.width, value.height);
                true
            }
        }
    }

    pub fn toggle_fullscreen(target: Target) {
        match target {
            Target::App => {}
            Target::Surface(v) => gobj(v).toggle_fullscreen(),
        }
    }

    pub fn toggle_quick_terminal(self_: &Application) -> bool {
        // If we already have a quick terminal window, we just toggle the
        // visibility of it.
        if let Some(win) = get_quick_terminal_window() {
            win.toggle_visibility();
            return true;
        }

        // If we don't support quick terminals then we do nothing.
        if !self_.with_winproto(|wp| wp.supports_quick_terminal()).unwrap_or(false) {
            return false;
        }

        // Create our new window as a quick terminal
        let win: Window = glib::Object::builder()
            .property("application", self_)
            .property("quick-terminal", true)
            .build();
        debug_assert!(win.is_quick_terminal());
        init_and_show_window(self_, &win, None, &Overrides::NONE);
        true
    }

    pub fn toggle_split_zoom(target: Target) -> bool {
        match target {
            Target::App => {
                log::warn!(target: LOG, "toggle_split_zoom to app is unexpected");
                false
            }

            Target::Surface(core) => {
                // TODO: pass surface ID when we have that
                let surface = gobj(core);
                let Some(tree) = ext::get_ancestor::<SplitTree>(&surface) else {
                    log::warn!(target: LOG, "surface is not in a split tree, ignoring toggle_split_zoom");
                    return false;
                };

                // If the tree has no splits (only one leaf), this action is
                // not performable. This allows the key event to pass through
                // to the terminal.
                if !tree.get_is_split() {
                    return false;
                }

                surface.activate_action("split-tree.zoom", None).is_ok()
            }
        }
    }

    pub fn show_on_screen_keyboard(target: Target) -> bool {
        match target {
            Target::App => {
                log::warn!(target: LOG, "show_on_screen_keyboard to app is unexpected");
                false
            }
            // NOTE: Even though `activateOsk` takes a gdk.Event, it's
            // currently unused by all implementations of `activateOsk` as of
            // GTK 4.18. The commit that introduced the method (ce6aa73c)
            // clarifies that the event *may* be used by other IM backends,
            // but for Linux desktop environments this doesn't matter.
            Target::Surface(v) => gobj(v).show_on_screen_keyboard(None),
        }
    }

    fn get_quick_terminal_window() -> Option<Window> {
        // Find a quick terminal window.
        gtk4::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<Window>().ok())
            .find(|w| w.is_quick_terminal())
    }

    pub fn toggle_maximize(target: Target) {
        match target {
            Target::App => {}
            Target::Surface(v) => gobj(v).toggle_maximize(),
        }
    }

    pub fn toggle_tab_overview(target: Target) -> bool {
        match target {
            Target::App => false,
            Target::Surface(core) => {
                let surface = gobj(core);
                let Some(window) = ext::get_ancestor::<Window>(&surface) else {
                    log::warn!(target: LOG, "surface is not in a window, ignoring new_tab");
                    return false;
                };

                window.toggle_tab_overview();
                true
            }
        }
    }

    pub fn toggle_window_decorations(target: Target) -> bool {
        match target {
            Target::App => false,
            Target::Surface(core) => {
                let surface = gobj(core);
                let Some(window) = ext::get_ancestor::<Window>(&surface) else {
                    log::warn!(target: LOG, "surface is not in a window, ignoring toggle_window_decorations");
                    return false;
                };

                window.toggle_window_decorations();
                true
            }
        }
    }

    pub fn toggle_command_palette(target: Target) -> bool {
        match target {
            Target::App => false,
            Target::Surface(surface) => gobj(surface).toggle_command_palette(),
        }
    }

    pub fn control_inspector(target: Target, value: Inspector) -> bool {
        match target {
            Target::App => false,
            Target::Surface(surface) => gobj(surface).control_inspector(value),
        }
    }

    pub fn command_finished(target: Target, value: CommandFinished) -> bool {
        match target {
            Target::App => false,
            Target::Surface(surface) => gobj(surface).command_finished(value),
        }
    }

    pub fn set_readonly(target: Target, value: Readonly) -> bool {
        match target {
            Target::App => false,
            Target::Surface(surface) => gobj(surface).set_readonly(value),
        }
    }

    pub fn key_sequence(target: Target, value: &KeySequence) -> bool {
        match target {
            Target::App => {
                log::warn!(target: LOG, "key_sequence action to app is unexpected");
                false
            }
            Target::Surface(core) => {
                gobj(core).key_sequence_action(value);
                true
            }
        }
    }

    pub fn key_table(target: Target, value: &KeyTable<'_>) -> bool {
        match target {
            Target::App => {
                log::warn!(target: LOG, "key_table action to app is unexpected");
                false
            }
            Target::Surface(core) => {
                gobj(core).key_table_action(value);
                true
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    thread_local! {
        static APP: RefCell<Option<Application>> = const { RefCell::new(None) };
    }

    /// A test application (on the GTK test thread) with a leaked core app
    /// and recording runtime app, set as the default application.
    pub fn default_app() -> Application {
        APP.with(|a| {
            if let Some(app) = a.borrow().as_ref() {
                return app.clone();
            }

            let core = Box::leak(CoreApp::create().expect("core app"));
            // The test config is not finalized (gtk-single-instance stays
            // `detect`), so keep the cgroup hooks out of child processes.
            let config = CoreConfig {
                linux_cgroup: crate::config::config::LinuxCgroup::Never,
                ..CoreConfig::default()
            };
            let app = Application::new_with_config(core, config, None);
            let rt = Box::leak(Box::new(apprt::App::default()));
            app.set_rt(rt);
            app.set_default();
            a.replace(Some(app.clone()));
            app
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apprt::gtk::ext::test_gtk;

    #[test]
    fn gtk_env() {
        assert_eq!(gtk_env_values(true, true), ("opengl".to_owned(), "vulkan".to_owned()));
        assert_eq!(gtk_env_values(false, true), (String::new(), "vulkan".to_owned()));
        assert_eq!(gtk_env_values(true, false), (String::new(), "vulkan".to_owned()));
    }

    #[test]
    fn runtime_css_contents() {
        let mut config = CoreConfig::default();
        let css = runtime_css(&config);
        assert!(css.contains(".child-exited.normal revealer widget"));
        assert!(css.contains("widget.unfocused-split {\n opacity: 0.30;"));
        assert!(!css.contains("--ghostty-fg"));

        config.window_theme = WindowTheme::Ghostty;
        config.split_divider_color = Some(ConfigColor { r: 1, g: 2, b: 3 });
        let css = runtime_css(&config);
        assert!(css.contains("--ghostty-bg: rgb(40,44,52);"));
        assert!(css.contains("color: rgb(1,2,3);"));
    }

    #[test]
    fn application_default_smoke() {
        let Some(()) = test_gtk::run(|| {
            let app = Application::default();
            assert_eq!(Application::try_default().as_ref(), Some(&app));
            assert!(app.get_config().is_some());
            assert!(app.with_winproto(|_| ()).is_some());

            // Unhandled actions report false, app-targeted surface actions
            // are rejected.
            assert!(!app.perform_action(Target::App, RtAction::Undo).unwrap());
            assert!(!app.perform_action(Target::App, RtAction::NewTab).unwrap());
            assert!(!app.perform_action(Target::App, RtAction::CloseWindow).unwrap());
        }) else {
            return;
        };
    }
}
