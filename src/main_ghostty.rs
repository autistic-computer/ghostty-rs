//! Port of `src/main_ghostty.zig`.
//!
//! STATUS: ported
//!
//! The main entrypoint for the `ghostty` application. This also serves
//! as the process initialization code for the `libghostty` library.
//!
//! Rust notes:
//! * Zig's `std_options.logFn` is the [`Logger`] installed as the `log`
//!   crate's logger. The macOS unified logging branch (and its cached
//!   `macosLogger`) only exists on macOS, where the Rust port builds the
//!   library rather than this executable; it is not ported.
//! * The application runtime is GTK when built with the `gtk` Cargo
//!   feature; without it the runtime is `none` and `ghostty` is only the
//!   helper CLI (like Zig's `app_runtime == .none`).
//! * `MainReturn` (noreturn for the lib artifact) is not needed: the lib's C
//!   API runs CLI actions through its own entry point.

use std::io::Write as _;
use std::sync::OnceLock;

use crate::app::App;
use crate::apprt;
use crate::global;

/// Errors from [`main`] (Zig: the inferred error set of `main`).
#[derive(Debug)]
pub enum MainError {
    /// The core app could not be created.
    App(crate::app::CreateError),
    /// The runtime app failed.
    Apprt(String),
}

impl std::fmt::Display for MainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MainError::App(e) => write!(f, "{e:?}"),
            MainError::Apprt(e) => f.write_str(e),
        }
    }
}

impl std::error::Error for MainError {}

pub fn main() -> Result<(), MainError> {
    // We first start by initializing our global state. This will setup
    // process-level state we need to run the terminal. The reason we use
    // a global is because the C API needs to be able to access this state;
    // no other code should EVER access the global state.
    install_logger();
    if let Err(err) = global::init(global::InitOpts::Main(global::Minimal::from_process())) {
        let mut stderr = std::io::stderr().lock();
        let _ = match err {
            global::InitError::MultipleActions => stderr.write_all(
                b"Error: multiple CLI actions specified. You must specify only one\n\
                  action starting with the `+` character.\n",
            ),

            global::InitError::InvalidAction => stderr.write_all(
                b"Error: unknown CLI action specified. CLI actions are specified with\n\
                  the '+' character.\n\n\
                  All valid CLI actions can be listed with `ghostty +help`\n",
            ),

            err => writeln!(stderr, "invalid CLI invocation err={err:?}"),
        };
        let _ = stderr.flush();
        std::process::exit(1);
    }
    struct Deinit;
    impl Drop for Deinit {
        fn drop(&mut self) {
            global::deinit();
        }
    }
    let _deinit = Deinit;

    if cfg!(debug_assertions) {
        log::warn!("This is a debug build. Performance will be very poor.");
        log::warn!("You should only use a debug build for developing Ghostty.");
        log::warn!("Otherwise, please rebuild in a release mode.");
    }

    // Execute our action if we have one
    if let Some(action) = global::action() {
        log::info!("executing CLI action={action:?}");
        let code = match action.run() {
            Ok(code) => code,
            Err(err) => {
                log::error!("CLI action failed error={err}");
                1
            }
        };
        std::process::exit(code as i32);
    }

    if !cfg!(feature = "gtk") {
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(b"Usage: ghostty +<action> [flags]\n\n");
        let _ = stdout.write_all(
            b"This is the Ghostty helper CLI that accompanies the graphical Ghostty app.
To launch the terminal directly, please launch the graphical app
(i.e. Ghostty.app on macOS). This CLI can be used to perform various
actions such as inspecting the version, listing fonts, etc.

On macOS, the terminal can also be launched using `open -na Ghostty.app`,
or `open -na Ghostty.app --args --foo=bar --baz=qux` to pass arguments.

We don't have proper help output yet, sorry! Please refer to the
source code or Discord community for help for now. We'll fix this in time.
",
        );
        let _ = stdout.flush();
        std::process::exit(0);
    }

    run_app()
}

/// Create the core app and the runtime app and run the GUI event loop.
#[cfg(feature = "gtk")]
fn run_app() -> Result<(), MainError> {
    // Create our app state
    let mut app: Box<App> = App::create().map_err(MainError::App)?;

    let result = (|| {
        // Create our runtime app
        let app_runtime = apprt::gtk::App::init(&mut app, apprt::gtk::app::Options::default())
            .map_err(|e| MainError::Apprt(e.to_string()))?;

        // Since - by definition - there are no surfaces when first started,
        // the quit timer may need to be started. The start timer will get
        // cancelled if/when the first surface is created. (The GTK runtime
        // has no `startQuitTimer`.)

        // Run the GUI event loop
        let rt = apprt::gtk::rt_app(&app_runtime);
        let result = rt.run().map_err(|e| MainError::Apprt(e.to_string()));
        rt.terminate();
        result
    })();

    app.destroy();
    result
}

/// See the GTK variant; without a GUI runtime `main` exits before this.
#[cfg(not(feature = "gtk"))]
fn run_app() -> Result<(), MainError> {
    let _ = App::create;
    let _: Option<apprt::App> = None;
    Ok(())
}

/// The function `log` will call (Zig: `logFn`).
pub struct Logger;

impl Logger {
    /// Our log level is always at least info in every build mode.
    ///
    /// Note, we don't lower this to debug even with conditional logging
    /// via GHOSTTY_LOG because our debug logs are very expensive to
    /// calculate and we want to make sure they're optimized out in
    /// builds.
    pub const LEVEL: log::LevelFilter = if cfg!(debug_assertions) {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Info
    };

    /// Zig's `std.log.Level.asText`.
    fn level_text(level: log::Level) -> &'static str {
        match level {
            log::Level::Error => "error",
            log::Level::Warn => "warning",
            log::Level::Info => "info",
            log::Level::Debug | log::Level::Trace => "debug",
        }
    }

    /// Formats one log line like Zig's stderr logger.
    pub fn format(level: log::Level, target: &str, args: &std::fmt::Arguments<'_>) -> String {
        // Targets with a module path (`log`'s default when no explicit
        // target is given) are Zig's `.default` scope.
        let prefix = if target.is_empty() || target.contains("::") || target == "ghostty" {
            ": ".to_owned()
        } else {
            format!("({target}): ")
        };
        format!("{}{}{}\n", Self::level_text(level), prefix, args)
    }
}

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= Self::LEVEL
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }

        // don't log debug messages to stderr unless we are a debug build
        if !cfg!(debug_assertions) && record.level() >= log::Level::Debug {
            return;
        }

        // skip if we are not logging to stderr (before global init we
        // assume the default, which is to log)
        let stderr_enabled = global::try_logging().map(|l| l.stderr).unwrap_or(true);
        if !stderr_enabled {
            return;
        }

        // Lock so we are thread-safe
        let line = Self::format(record.level(), record.target(), record.args());
        let mut stderr = std::io::stderr().lock();
        let _ = stderr.write_all(line.as_bytes());
        let _ = stderr.flush();
    }

    fn flush(&self) {}
}

/// Install [`Logger`] as the process logger (once).
pub fn install_logger() {
    static LOGGER: Logger = Logger;
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        if log::set_logger(&LOGGER).is_ok() {
            log::set_max_level(Logger::LEVEL);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_format() {
        let line = Logger::format(log::Level::Warn, "gtk", &format_args!("hello {}", 1));
        assert_eq!(line, "warning(gtk): hello 1\n");
        let line = Logger::format(log::Level::Info, "ghostty::app", &format_args!("x"));
        assert_eq!(line, "info: x\n");
        let line = Logger::format(log::Level::Debug, "surface", &format_args!("y"));
        assert_eq!(line, "debug(surface): y\n");
    }

    #[test]
    fn log_level() {
        if cfg!(debug_assertions) {
            assert_eq!(Logger::LEVEL, log::LevelFilter::Debug);
        } else {
            assert_eq!(Logger::LEVEL, log::LevelFilter::Info);
        }
    }
}
