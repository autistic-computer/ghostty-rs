//! Port of `src/termio/Termio.zig`.
//!
//! STATUS: ported
//!
//! Primary terminal IO ("termio") state. This maintains the terminal state,
//! pty, subprocess, etc. This is flexible enough to be used in environments
//! that don't have a pty and simply provides the input/output using raw
//! bytes.
//!
//! Deviations from the Zig code:
//! * Zig initializes the termio in place (`init(self: *Termio, ...)`) because
//!   the stream handler points into it. [`Termio::init`] returns a `Box` so
//!   that those pointers (and `renderer_state.terminal`, which the owner
//!   points at [`Termio::terminal`]) stay valid.
//! * Like Zig, the termio is shared by raw pointer between the IO thread
//!   (which owns the backend and drains the mailbox) and the backend's read
//!   thread (which only calls [`Termio::process_output`]); every piece of
//!   state both touch is protected by the renderer state mutex.
//! * Allocators and arenas are dropped; [`DerivedConfig`] owns its values.
//!   [`Termio::change_config`] takes the new config by value (Zig copies it
//!   out of a pointer).
//! * The error sets are the [`Error`] enum.
//! * `last_cursor_reset` is a monotonic [`Instant`].

use std::fmt;
use std::io::{self, Cursor, Read as _};
use std::time::Instant;

use crate::apprt;
use crate::config::config::{
    Color as ConfigColor, GraphemeWidthMethod, OSCColorReportFormat, TerminalColor,
    XtChecksumExtension,
};
use crate::config::io::{ReadableIO, RepeatableReadableIO};
use crate::config::{ClipboardAccess, Config, ConditionalState};
use crate::datastruct::blocking_queue::Timeout as QueueTimeout;
use crate::global;
use crate::global::xev;
use crate::lib::tinyio::{Mutex, TinyIo};
use crate::pty::{ProcessInfo, ProcessInfoValue};
use crate::renderer;
use crate::terminal::color::{self as terminal_color, DynamicPalette, DynamicRgb};
use crate::terminal::csi;
use crate::terminal::device_status;
use crate::terminal::focus;
use crate::terminal::kitty::clipboard_grants::Direction as KittyClipboardDirection;
use crate::terminal::kitty::graphics_command::DeleteAction;
use crate::terminal::kitty::graphics_exec::terminal_info;
use crate::terminal::kitty::graphics_image::Limits as LoadingImageLimits;
use crate::terminal::modes::{Mode, ModePacked};
use crate::terminal::screen::Scroll as ScreenScroll;
use crate::terminal::screen_set::Key as ScreenKey;
use crate::terminal::size_report;
use crate::terminal::stream::Options as StreamOptions;
use crate::terminal::terminal::{
    CellSizePx, Colors, InitError, Options as TerminalOptions, Resize as TerminalResize,
    ResizeError, ScrollViewport,
};
use crate::terminal::{osc, xt_checksum, CursorStyle, Terminal};
use crate::termio::backend;
use crate::termio::exec;
use crate::termio::message::SizeReport;
use crate::termio::stream_handler::{Stream, StreamHandler};
use crate::termio::{Backend, Mailbox, Message, Options, Thread};

const LOG: &str = "io_exec";

/// Mutex state argument for queueMessage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutexState {
    Locked,
    Unlocked,
}

/// The errors from termio operations (Zig: the inferred error sets of the
/// `Termio` methods, which include the backend and terminal errors).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// An error from the exec backend.
    Exec(exec::Error),

    /// An error from the event loop.
    Xev(xev::Error),

    /// A configured `input` path was not found.
    InputNotFound,

    /// A configured `input` could not be read or written.
    InputFailed,

    OutOfMemory,

    /// Invalid terminal dimensions.
    InvalidValue,

    /// Encoding a report failed.
    WriteFailed,

    Unexpected,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Exec(e) => fmt::Display::fmt(e, f),
            Error::Xev(e) => write!(f, "{e:?}"),
            other => fmt::Debug::fmt(other, f),
        }
    }
}

impl std::error::Error for Error {}

impl From<exec::Error> for Error {
    fn from(e: exec::Error) -> Self {
        Error::Exec(e)
    }
}

impl From<xev::Error> for Error {
    fn from(e: xev::Error) -> Self {
        Error::Xev(e)
    }
}

impl From<InitError> for Error {
    fn from(e: InitError) -> Self {
        match e {
            InitError::OutOfMemory => Error::OutOfMemory,
            InitError::InvalidValue => Error::InvalidValue,
        }
    }
}

impl From<ResizeError> for Error {
    fn from(e: ResizeError) -> Self {
        match e {
            ResizeError::OutOfMemory => Error::OutOfMemory,
            ResizeError::InvalidValue => Error::InvalidValue,
        }
    }
}

impl From<io::Error> for Error {
    fn from(_: io::Error) -> Self {
        Error::WriteFailed
    }
}

/// The temporary directory for Kitty image loading (Zig:
/// `global.tmpDirPath()`). Falls back to the OS temporary directory when
/// the global state is not initialized (tests, embedders).
fn tmp_dir_path() -> String {
    global::try_tmp_dir_path()
        .unwrap_or_else(|| std::env::temp_dir().to_string_lossy().into_owned())
}

/// Primary terminal IO state (see the module documentation).
pub struct Termio {
    /// This is the implementation responsible for io.
    pub backend: Backend,

    /// The derived configuration for this termio implementation.
    pub config: DerivedConfig,

    /// The terminal emulator internal state. This is the abstract "terminal"
    /// that manages input, grid updating, etc. and is renderer-agnostic. It
    /// just stores internal state about a grid.
    pub terminal: Terminal,

    /// The shared render state
    pub renderer_state: *mut renderer::State,

    /// A handle to wake up the renderer. This hints to the renderer that
    /// a repaint should happen.
    pub renderer_wakeup: xev::Async,

    /// The mailbox for notifying the renderer of things.
    pub renderer_mailbox: *const renderer::thread::Mailbox,

    /// The mailbox for communicating with the surface.
    pub surface_mailbox: apprt::surface::Mailbox,

    /// The cached size info
    pub size: renderer::Size,

    /// The mailbox implementation to use.
    pub mailbox: Mailbox,

    /// The stream parser. This parses the stream of escape codes and so on
    /// from the child process and calls callbacks in the stream handler.
    pub terminal_stream: Stream,

    /// Last time the cursor was reset. This is used to prevent message
    /// flooding with cursor resets.
    pub last_cursor_reset: Option<Instant>,

    /// State we have for thread enter. This may be null if we don't need
    /// to keep track of any state or if its already been freed.
    pub thread_enter_state: Option<Box<ThreadEnterState>>,
}

// SAFETY: see the module docs; the raw pointers reference objects that
// outlive the termio and are synchronized like in Zig.
unsafe impl Send for Termio {}

impl fmt::Debug for Termio {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Termio")
            .field("backend", &self.backend)
            .field("size", &self.size)
            .field("mailbox", &self.mailbox)
            .finish_non_exhaustive()
    }
}

/// The state we need to keep around only until we enter the IO
/// thread. Then we can throw it all away.
#[derive(Debug)]
pub struct ThreadEnterState {
    /// Initial input to send to the subprocess after starting. This
    /// memory is freed once the subprocess start is attempted, even
    /// if it fails, because Exec only starts once.
    pub input: RepeatableReadableIO,
}

/// A prepared initial input (Zig: `ThreadEnterState.Input`).
#[derive(Debug)]
pub enum ThreadEnterInput<'a> {
    String(&'a [u8]),
    File(std::fs::File),
}

impl ThreadEnterState {
    pub fn create(config: &Config) -> Option<Box<ThreadEnterState>> {
        // If we have no input then we have no thread enter state
        if config.input.list.is_empty() {
            return None;
        }

        // Copy the input from the config
        Some(Box::new(ThreadEnterState {
            input: config.input.clone_parsed(),
        }))
    }

    pub fn destroy(self: Box<Self>) {
        drop(self);
    }

    /// Prepare the inputs for use.
    pub fn prepare_input(&self) -> Result<Vec<ThreadEnterInput<'_>>, Error> {
        let mut inputs: Vec<ThreadEnterInput<'_>> = Vec::with_capacity(self.input.list.len());

        for item in &self.input.list {
            inputs.push(match item {
                ReadableIO::Raw(v) => ThreadEnterInput::String(v.as_bytes()),
                ReadableIO::Path(path) => {
                    let path: &str = path;
                    match std::fs::File::open(path) {
                        Ok(f) => ThreadEnterInput::File(f),
                        Err(err) => {
                            log::warn!(target: LOG, "failed to open input file={path} err={err}");
                            return Err(Error::InputNotFound);
                        }
                    }
                }
            });
        }

        Ok(inputs)
    }
}

/// Convert the vt-xt-checksum-extension config, whose values are named for
/// what they turn on, to the XTCHECKSUM bits, which turn things off.
fn xt_checksum_flags(ext: XtChecksumExtension) -> xt_checksum::Flags {
    xt_checksum::Flags {
        positive: !ext.negate,
        no_attributes: !ext.attributes,
        no_trim: !ext.trim,
        undrawn: ext.undrawn,
        full: ext.full,
    }
}

/// The configuration for this IO that is derived from the main
/// configuration. This must be exported so that we don't need to
/// pass around Config pointers which makes memory management a pain.
#[derive(Debug, Clone)]
pub struct DerivedConfig {
    pub palette: terminal_color::Palette,
    pub image_storage_limit: usize,
    pub cursor_style: CursorStyle,
    pub cursor_blink: Option<bool>,
    pub cursor_color: Option<TerminalColor>,
    pub foreground: ConfigColor,
    pub background: ConfigColor,
    pub osc_color_report_format: OSCColorReportFormat,
    pub clipboard_write: ClipboardAccess,
    pub clipboard_write_limit: usize,
    pub enquiry_response: Vec<u8>,
    pub xt_checksum_report: bool,
    pub xt_checksum_extension: xt_checksum::Flags,
    pub conditional_state: ConditionalState,
}

impl DerivedConfig {
    pub fn init(config: &Config) -> DerivedConfig {
        let palette: terminal_color::Palette = 'palette: {
            if config.palette_generate {
                'generate: {
                    if config.palette.mask.iter().next().is_none() {
                        // If the user didn't set any values manually, then
                        // we're using the default palette and we don't need
                        // to apply the generation code to it.
                        break 'generate;
                    }

                    break 'palette terminal_color::generate_256_color(
                        config.palette.value,
                        config.palette.mask,
                        config.background.to_terminal_rgb(),
                        config.foreground.to_terminal_rgb(),
                        config.palette_harmonious,
                    );
                }
            }

            config.palette.value
        };

        DerivedConfig {
            palette,
            image_storage_limit: config.image_storage_limit as usize,
            cursor_style: config.cursor_style,
            cursor_blink: config.cursor_style_blink,
            cursor_color: config.cursor_color,
            foreground: config.foreground,
            background: config.background,
            osc_color_report_format: config.osc_color_report_format,
            clipboard_write: config.clipboard_write,
            clipboard_write_limit: config.clipboard_write_limit_bytes.value,
            enquiry_response: config.enquiry_response.as_bytes().to_vec(),
            xt_checksum_report: config.vt_xt_checksum_report,
            xt_checksum_extension: xt_checksum_flags(config.vt_xt_checksum_extension),
            conditional_state: config._conditional_state,
        }
    }

    pub fn deinit(&mut self) {}
}

impl Termio {
    /// Initialize the termio state.
    ///
    /// This will also start the child process if the termio is configured
    /// to run a child process.
    pub fn init(opts: Options) -> Result<Box<Termio>, Error> {
        // SAFETY: the full config is only valid (and only read) during init.
        let full_config = unsafe { &*opts.full_config };

        // The default terminal modes based on our config.
        let default_modes: ModePacked = {
            let mut modes = ModePacked::default();

            // Setup our initial grapheme cluster support if enabled. We use a
            // switch to ensure we get a compiler error if more cases are added.
            match full_config.grapheme_width_method {
                GraphemeWidthMethod::Unicode => modes.set(Mode::GraphemeCluster, true),
                GraphemeWidthMethod::Legacy => {}
            }

            // Set default cursor blink settings
            modes.set(Mode::CursorBlinking, opts.config.cursor_blink.unwrap_or(true));

            modes
        };

        // Create our terminal
        let mut term = Terminal::init({
            let grid_size = opts.size.grid();
            TerminalOptions {
                cols: grid_size.columns,
                rows: grid_size.rows,
                max_scrollback_bytes: full_config.scrollback_limit_bytes.optional(),
                max_scrollback_lines: full_config.scrollback_limit_lines.optional(),
                default_modes,
                default_cursor_style: opts.config.cursor_style,
                default_cursor_blink: opts.config.cursor_blink,
                default_xt_checksum: opts.config.xt_checksum_extension,
                colors: Colors {
                    background: DynamicRgb::init(opts.config.background.to_terminal_rgb()),
                    foreground: DynamicRgb::init(opts.config.foreground.to_terminal_rgb()),
                    cursor: 'cursor: {
                        let Some(color) = opts.config.cursor_color else {
                            break 'cursor DynamicRgb::UNSET;
                        };
                        let Some(rgb) = color.to_terminal_rgb() else {
                            break 'cursor DynamicRgb::UNSET;
                        };
                        DynamicRgb::init(rgb)
                    },
                    palette: DynamicPalette::DEFAULT,
                },
                kitty_image_storage_limit: opts.config.image_storage_limit,
                kitty_image_loading_limits: LoadingImageLimits::all_with_temp_dir(
                    tmp_dir_path().as_bytes(),
                ),
            }
        })?;

        // The default palette may be an allocator-owned copy, so it is set
        // once the terminal owns its memory and can release it on deinit.
        term.colors.palette.change_default(opts.config.palette);

        // Setup our terminal size in pixels for certain requests.
        term.width_px = term.cols as u32 * opts.size.cell.width;
        term.height_px = term.rows as u32 * opts.size.cell.height;

        // Setup our backend.
        let mut backend = opts.backend;
        backend.init_terminal(&mut term);

        // Create our stream handler. This points to memory in self so it
        // isn't safe to use until self is boxed (the pointers are set below).
        let handler = StreamHandler {
            termio_mailbox: std::ptr::null(),
            surface_mailbox: opts.surface_mailbox,
            renderer_state: opts.renderer_state,
            renderer_wakeup: opts.renderer_wakeup,
            renderer_mailbox: opts.renderer_mailbox,
            size: std::ptr::null(),
            terminal: std::ptr::null_mut(),
            osc_color_report_format: opts.config.osc_color_report_format,
            clipboard_write: opts.config.clipboard_write,
            clipboard_write_limit: opts.config.clipboard_write_limit,
            enquiry_response: opts.config.enquiry_response.clone(),
            xt_checksum_report: opts.config.xt_checksum_report,
            apc: Default::default(),
            dcs: Default::default(),
            tmux_viewer: None,
            kitty_clipboard_grants: Default::default(),
            kitty_clipboard_write: None,
            termio_messaged: false,
            seen_title: false,
        };

        let thread_enter_state = ThreadEnterState::create(full_config);

        let mut this = Box::new(Termio {
            terminal: term,
            config: opts.config,
            renderer_state: opts.renderer_state,
            renderer_wakeup: opts.renderer_wakeup,
            renderer_mailbox: opts.renderer_mailbox,
            surface_mailbox: opts.surface_mailbox,
            size: opts.size,
            backend,
            mailbox: opts.mailbox,
            terminal_stream: Stream::init(StreamOptions {
                allocator: Some(osc::Allocator::GLOBAL),
                ..StreamOptions::new(handler)
            }),
            last_cursor_reset: None,
            thread_enter_state,
        });

        // Point the handler at our (now pinned) memory.
        let handler = &mut this.terminal_stream.handler;
        handler.termio_mailbox = &this.mailbox;
        handler.size = &this.size;
        handler.terminal = &mut this.terminal;

        Ok(this)
    }

    pub fn deinit(&mut self) {
        self.backend.deinit();
        self.config.deinit();
        self.mailbox.deinit();

        // Clear any StreamHandler state
        self.terminal_stream.deinit();

        // Clear any initial state if we have it
        if let Some(v) = self.thread_enter_state.take() {
            v.destroy();
        }
    }

    fn mutex(&self) -> &Mutex {
        // SAFETY: the render state and its mutex outlive the termio.
        unsafe { (*self.renderer_state).mutex.as_ref() }
    }

    fn lock(&self) {
        self.mutex().lock_uncancelable(TinyIo);
    }

    fn unlock(&self) {
        self.mutex().unlock(TinyIo);
    }

    fn renderer_mailbox(&self) -> &renderer::thread::Mailbox {
        // SAFETY: the renderer mailbox outlives the termio.
        unsafe { &*self.renderer_mailbox }
    }

    /// The terminal the renderer reads (Zig: `renderer_state.terminal`).
    fn state_terminal(&self) -> &Terminal {
        // SAFETY: the owner points the render state at a live terminal and
        // we hold the renderer state mutex while reading it.
        unsafe { (*self.renderer_state).terminal.as_ref() }
    }

    pub fn thread_enter(&mut self, thread: *mut Thread, data: &mut ThreadData) -> Result<(), Error> {
        // Always free our thread enter state when we're done.
        let thread_enter_state = self.thread_enter_state.take();

        // If we have thread enter state then we're going to validate
        // and set that all up now so that we can error before we actually
        // start the command and pty.
        let inputs: Option<Vec<ThreadEnterInput<'_>>> = match &thread_enter_state {
            Some(v) => Some(v.prepare_input()?),
            None => None,
        };

        *data = ThreadData {
            // SAFETY: the thread outlives its thread data.
            r#loop: unsafe { &mut (*thread).r#loop },
            renderer_state: self.renderer_state,
            surface_mailbox: self.surface_mailbox,
            mailbox: &mut self.mailbox,
            // Backend must replace this on threadEnter
            backend: backend::ThreadData::Exec(exec::ThreadData::default()),
        };

        // Setup our backend
        let io: *mut Termio = self;
        self.backend.thread_enter(io, data)?;

        // If we have inputs, then queue them all up.
        let res = (|| -> Result<(), Error> {
            for input in inputs.unwrap_or_default() {
                match input {
                    ThreadEnterInput::String(v) => {
                        if let Err(err) = self.queue_write(data, v, false) {
                            log::warn!(target: LOG, "failed to queue input string err={err}");
                            return Err(Error::InputFailed);
                        }
                    }
                    ThreadEnterInput::File(f) => {
                        let contents = match read_to_end_alloc(f, 10 * 1024 * 1024) {
                            // 10 MiB max
                            Ok(v) => v,
                            Err(err) => {
                                log::warn!(target: LOG, "failed to read input file err={err}");
                                return Err(Error::InputFailed);
                            }
                        };

                        if let Err(err) = self.queue_write(data, &contents, false) {
                            log::warn!(target: LOG, "failed to queue input file err={err}");
                            return Err(Error::InputFailed);
                        }
                    }
                }
            }
            Ok(())
        })();

        if res.is_err() {
            self.backend.thread_exit(data);
        }

        if let Some(v) = thread_enter_state {
            v.destroy();
        }

        res
    }

    pub fn thread_exit(&mut self, data: &mut ThreadData) {
        self.backend.thread_exit(data);
    }

    /// Send a message to the mailbox. Depending on the mailbox type in use
    /// this may process now or it may just enqueue and process later.
    ///
    /// This will also notify the mailbox thread to process the message. If
    /// you're sending a lot of messages, it may be more efficient to use
    /// the mailbox directly and then call notify separately.
    pub fn queue_message(&mut self, msg: Message, mutex: MutexState) {
        let m = match mutex {
            MutexState::Locked => Some(self.mutex()),
            MutexState::Unlocked => None,
        };
        self.mailbox.send(msg, m);
        self.mailbox.notify();
    }

    /// Queue a write directly to the pty.
    ///
    /// If you're using termio.Thread, this must ONLY be called from the
    /// mailbox thread. If you're not on the thread, use queueMessage with
    /// mailbox messages instead.
    ///
    /// If you're not using termio.Thread, this is not threadsafe.
    #[inline]
    pub fn queue_write(&mut self, td: &mut ThreadData, data: &[u8], linefeed: bool) -> Result<(), Error> {
        self.backend.queue_write(td, data, linefeed)?;
        Ok(())
    }

    /// Update the configuration.
    pub fn change_config(&mut self, td: &mut ThreadData, config: DerivedConfig) -> Result<(), Error> {
        // The remainder of this function is modifying terminal state or
        // the read thread data, all of which requires holding the renderer
        // state lock.
        self.lock();

        // Deinit our old config. We do this in the lock because the
        // stream handler may be referencing the old config (i.e. enquiry resp)
        self.config.deinit();
        self.config = config;

        // Update our stream handler. The stream handler uses the same
        // renderer mutex so this is safe to do despite being executed
        // from another thread.
        self.terminal_stream.handler.change_config(&self.config);
        td.backend.change_config(&mut self.config);

        // Update the configuration that we know about.
        //
        // Specific things we don't update:
        //   - command, working-directory: we never restart the underlying
        //   process so we don't care or need to know about these.

        // Update the default palette. (Zig falls back to the built-in
        // default if allocating the copy fails; the copy can't fail here.)
        let config = &self.config;
        let t = &mut self.terminal;
        t.colors.palette.change_default(config.palette);
        t.flags.dirty.palette = true;

        // Update all our other colors
        t.colors.background.default = Some(config.background.to_terminal_rgb());
        t.colors.foreground.default = Some(config.foreground.to_terminal_rgb());
        t.colors.cursor.default = config.cursor_color.and_then(|c| c.to_terminal_rgb());

        // Set the image limits
        t.set_kitty_graphics_size_limit(config.image_storage_limit);
        t.set_kitty_graphics_loading_limits(LoadingImageLimits::all_with_temp_dir(
            tmp_dir_path().as_bytes(),
        ));

        // The config could have changed any of our colors so update mode 2031.
        // This is written directly rather than queued as a color_scheme_report
        // message because we're on the io thread, the only reader of our
        // mailbox: if the mailbox were full, queueing would wait forever.
        if let Err(err) = self.color_scheme_report_locked(td, false) {
            log::warn!(target: LOG, "error sending color scheme report err={err}");
        }

        self.unlock();
        Ok(())
    }

    /// Resize the terminal.
    pub fn resize(&mut self, td: &mut ThreadData, size: renderer::Size) -> Result<(), Error> {
        self.size = size;
        let grid_size = size.grid();

        // Enter the critical area that we want to keep small
        {
            self.lock();
            let res = (|| -> Result<(), Error> {
                // Resizing the pty can immediately wake the shell with SIGWINCH.
                // Hold the same lock used by processOutput before notifying it, so
                // its redraw cannot be parsed against the old terminal dimensions.
                self.backend.resize(grid_size, size.terminal())?;

                // Update the size of our terminal state
                self.terminal.resize(TerminalResize {
                    cols: grid_size.columns,
                    rows: grid_size.rows,
                    cell_size_px: Some(CellSizePx {
                        width: self.size.cell.width,
                        height: self.size.cell.height,
                    }),
                })?;

                // If we have size reporting enabled we need to send a report.
                if self.terminal.modes.get(Mode::InBandSizeReports) {
                    self.size_report_locked(td, SizeReport::Mode2048)?;
                }
                Ok(())
            })();
            self.unlock();
            res?;
        }

        // Mail the renderer so that it can update the GPU and re-render
        let _ = self
            .renderer_mailbox()
            .push(renderer::Message::Resize(size), QueueTimeout::Forever);
        let _ = self.renderer_wakeup.notify();
        Ok(())
    }

    /// Make a size report.
    pub fn size_report(&mut self, td: &mut ThreadData, style: SizeReport) -> Result<(), Error> {
        self.lock();
        let res = self.size_report_locked(td, style);
        self.unlock();
        res
    }

    fn size_report_locked(&mut self, td: &mut ThreadData, style: SizeReport) -> Result<(), Error> {
        let grid_size = self.size.grid();
        let report_size = size_report::Size {
            rows: grid_size.rows,
            columns: grid_size.columns,
            cell_width: self.size.cell.width,
            cell_height: self.size.cell.height,
        };

        // 1024 bytes should be enough for size report since report
        // in columns and pixels.
        let mut buf = [0u8; 1024];
        let len = {
            let mut writer = Cursor::new(&mut buf[..]);
            size_report::encode(&mut writer, style, report_size)?;
            writer.position() as usize
        };

        self.queue_write(td, &buf[..len], false)
    }

    /// Reset the synchronized output mode. This is usually called by timer
    /// expiration from the termio thread.
    pub fn reset_synchronized_output(&mut self) {
        self.lock();
        self.terminal.modes.set(Mode::SynchronizedOutput, false);
        let _ = self.renderer_wakeup.notify();
        self.unlock();
    }

    /// Clear the screen.
    pub fn clear_screen(&mut self, td: &mut ThreadData, history: bool) -> Result<(), Error> {
        {
            self.lock();
            let at_prompt = 'locked: {
                let t = &mut self.terminal;

                // If we're on the alternate screen, we do not clear. Since this is an
                // emulator-level screen clear, this messes up the running programs
                // knowledge of where the cursor is and causes rendering issues. So,
                // for alt screen, we do nothing.
                if t.screens.active_key == ScreenKey::Alternate {
                    break 'locked false;
                }

                // Clear our selection
                t.screens.active.clear_selection();

                // Clear our scrollback
                if history {
                    t.erase_display(csi::EraseDisplay::Scrollback, false);
                }

                // If we're not at a prompt, we just delete above the cursor.
                if !t.cursor_is_at_prompt() {
                    if t.screens.active.cursor.y > 0 {
                        let y = t.screens.active.cursor.y - 1;
                        t.screens.active.erase_active(y);
                    }

                    // Clear all Kitty graphics state for this screen. This copies
                    // Kitty's behavior when Cmd+K deletes all Kitty graphics. I
                    // didn't spend time researching whether it only deletes Kitty
                    // graphics that are placed above the cursor or if it deletes
                    // all of them. We delete all of them for now but if this behavior
                    // isn't fully correct we should fix this later.
                    let info = terminal_info(t);
                    let screen = &mut *t.screens.active;
                    screen
                        .kitty_images
                        .delete(&mut screen.pages, &info, DeleteAction::All(true));

                    break 'locked false;
                }

                // At a prompt, we want to first fully clear the screen, and then after
                // send a FF (0x0C) to the shell so that it can repaint the screen.
                // Mark the current row as a not a prompt so we can properly
                // clear the full screen in the next eraseDisplay call.
                // TODO: fix this
                // self.terminal.markSemanticPrompt(.command);
                // assert(!self.terminal.cursorIsAtPrompt());
                t.erase_display(csi::EraseDisplay::Complete, false);
                true
            };
            self.unlock();

            if !at_prompt {
                return Ok(());
            }
        }

        // If we reached here it means we're at a prompt, so we send a form-feed.
        self.queue_write(td, &[0x0C], false)
    }

    /// Scroll the viewport
    pub fn scroll_viewport(&mut self, scroll: ScrollViewport) {
        self.lock();
        self.terminal.scroll_viewport(scroll);
        self.unlock();
    }

    /// Jump the viewport to the prompt.
    pub fn jump_to_prompt(&mut self, delta: isize) -> Result<(), Error> {
        {
            self.lock();
            self.terminal.screens.active.scroll(ScreenScroll::DeltaPrompt(delta));
            self.unlock();
        }

        self.renderer_wakeup.notify()?;
        Ok(())
    }

    /// Called when focus is gained or lost (when focus events are enabled)
    pub fn focus_gained(&mut self, td: &mut ThreadData, focused: bool) -> Result<(), Error> {
        self.lock();
        let focus_event = self.state_terminal().modes.get(Mode::FocusEvent);
        self.unlock();

        // If we have focus events enabled, we send the focus event.
        if focus_event {
            let mut buf = [0u8; focus::MAX_ENCODE_SIZE];
            let len = {
                let mut writer = Cursor::new(&mut buf[..]);
                let event = if focused {
                    focus::Event::Gained
                } else {
                    focus::Event::Lost
                };
                if let Err(err) = focus::encode(&mut writer, event) {
                    log::error!(target: LOG, "error encoding focus event err={err}");
                    return Ok(());
                }
                writer.position() as usize
            };
            self.queue_write(td, &buf[..len], false)?;
        }

        // We always notify our backend of focus changes.
        self.backend.focus_gained(td, focused)?;
        Ok(())
    }

    /// Process output from the pty. This is the manual API that users can
    /// call with pty data but it is also called by the read thread when using
    /// an exec subprocess.
    pub fn process_output(&mut self, buf: &[u8]) {
        // We are modifying terminal state from here on out and we need
        // the lock to grab our read data.
        self.lock();
        self.process_output_locked(buf);
        self.unlock();
    }

    /// Process output from readdata but the lock is already held.
    fn process_output_locked(&mut self, buf: &[u8]) {
        // Schedule a render. We can call this first because we have the lock.
        if self.terminal_stream.handler.queue_render().is_err() {
            unreachable!();
        }

        // Whenever a character is typed, we ensure the cursor is in the
        // non-blink state so it is rendered if visible. If we're under
        // HEAVY read load, we don't want to send a ton of these so we
        // use a timer under the covers
        let now = Instant::now();
        'cursor_reset: {
            if let Some(last) = self.last_cursor_reset {
                if now.saturating_duration_since(last).as_millis() <= 500 {
                    break 'cursor_reset;
                }
            }

            self.last_cursor_reset = Some(now);
            let _ = self
                .renderer_mailbox()
                .push(renderer::Message::ResetCursorBlink, QueueTimeout::Instant);
        }

        // If we have an inspector, we enter SLOW MODE because we need to
        // process a byte at a time alternating between the inspector handler
        // and the termio handler. This is very slow compared to our optimizations
        // below but at least users only pay for it if they're using the inspector.
        // SAFETY: the render state outlives the termio; the inspector is
        // protected by the renderer state mutex, which we hold.
        if let Some(mut insp) = unsafe { (*self.renderer_state).inspector } {
            for i in 0..buf.len() {
                // SAFETY: as above.
                let insp = unsafe { insp.as_mut() };
                if let Err(err) = insp.record_pty_read(&mut self.terminal, &buf[i..i + 1]) {
                    log::error!(target: LOG, "error recording pty read in inspector err={err}");
                }

                self.terminal_stream.next(buf[i]);
            }
        } else {
            self.terminal_stream.next_slice(buf);
        }

        // If our stream handling caused messages to be sent to the mailbox
        // thread, then we need to wake it up so that it processes them.
        if self.terminal_stream.handler.termio_messaged {
            self.terminal_stream.handler.termio_messaged = false;
            self.mailbox.notify();
        }
    }

    /// Record a Kitty clipboard protocol session grant so future requests
    /// carrying the password skip the permission prompt.
    pub fn kitty_clipboard_grant(&mut self, pw: &[u8], dir: KittyClipboardDirection) -> Result<(), Error> {
        self.lock();
        self.terminal_stream.handler.kitty_clipboard_grant(pw, dir);
        self.unlock();
        Ok(())
    }

    /// Sends a DSR response for the current color scheme to the pty.
    pub fn color_scheme_report(&mut self, td: &mut ThreadData, force: bool) -> Result<(), Error> {
        self.lock();
        let res = self.color_scheme_report_locked(td, force);
        self.unlock();
        res
    }

    pub fn color_scheme_report_locked(&mut self, td: &mut ThreadData, force: bool) -> Result<(), Error> {
        if !force && !self.state_terminal().modes.get(Mode::ReportColorScheme) {
            return Ok(());
        }
        let scheme = match self.config.conditional_state.theme {
            crate::config::conditional::Theme::Light => device_status::ColorScheme::Light,
            crate::config::conditional::Theme::Dark => device_status::ColorScheme::Dark,
        };

        let mut buf = [0u8; device_status::MAX_COLOR_SCHEME_REPORT_ENCODE_SIZE];
        let len = {
            let mut writer = Cursor::new(&mut buf[..]);
            device_status::encode_color_scheme_report(&mut writer, scheme)?;
            writer.position() as usize
        };
        self.queue_write(td, &buf[..len], false)
    }

    /// Sends a visibility report to the pty. Unforced reports are only sent while
    /// DEC mode 2033 is enabled.
    pub fn visibility_report(&mut self, td: &mut ThreadData, visible: bool, force: bool) -> Result<(), Error> {
        self.lock();
        let res = (|| -> Result<(), Error> {
            if !force && !self.state_terminal().modes.get(Mode::ReportVisibility) {
                return Ok(());
            }

            let mut buf = [0u8; device_status::MAX_VISIBILITY_REPORT_ENCODE_SIZE];
            let len = {
                let mut writer = Cursor::new(&mut buf[..]);
                device_status::encode_visibility_report(
                    &mut writer,
                    if visible {
                        device_status::Visibility::PotentiallyVisible
                    } else {
                        device_status::Visibility::NotVisible
                    },
                )?;
                writer.position() as usize
            };
            self.queue_write(td, &buf[..len], false)
        })();
        self.unlock();
        res
    }

    /// Get information about the process(es) attached to the backend. Returns
    /// `null` if there was an error getting the information or the information is
    /// not available on a particular platform.
    pub fn get_process_info(&mut self, info: ProcessInfo) -> Option<ProcessInfoValue<'_>> {
        self.backend.get_process_info(info)
    }
}

/// The crash-metadata surface for a surface mailbox (Zig:
/// `surface_mailbox.surface`).
pub fn crash_surface(mailbox: &apprt::surface::Mailbox) -> *const crate::surface::Surface {
    mailbox.surface
}

/// Read a whole file up to `max_bytes` (Zig:
/// `compat_file.readToEndAlloc`); larger files are an error.
fn read_to_end_alloc(f: std::fs::File, max_bytes: usize) -> io::Result<Vec<u8>> {
    let mut contents = Vec::new();
    f.take(max_bytes as u64 + 1).read_to_end(&mut contents)?;
    if contents.len() > max_bytes {
        return Err(io::Error::other("StreamTooLong"));
    }
    Ok(contents)
}

/// ThreadData is the data created and stored in the termio thread
/// when the thread is started and destroyed when the thread is
/// stopped.
///
/// All of the fields in this struct should only be read/written by
/// the termio thread. As such, a lock is not necessary. It lives on the IO
/// thread's stack for the lifetime of the thread and must not move while
/// the thread runs.
pub struct ThreadData {
    /// The event loop associated with this thread. This is owned by
    /// the Thread but we have a pointer so we can queue new work to it.
    pub r#loop: *mut xev::Loop,

    /// The shared render state
    pub renderer_state: *mut renderer::State,

    /// Mailboxes for different threads
    pub surface_mailbox: apprt::surface::Mailbox,

    /// Data associated with the backend implementation (i.e. pty/exec state)
    pub backend: backend::ThreadData,
    pub mailbox: *mut Mailbox,
}

impl fmt::Debug for ThreadData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ThreadData")
            .field("backend", &self.backend)
            .finish_non_exhaustive()
    }
}

/// The placeholder value before `Termio::thread_enter` fills it in
/// (Zig: `undefined`).
impl Default for ThreadData {
    fn default() -> Self {
        ThreadData {
            r#loop: std::ptr::null_mut(),
            renderer_state: std::ptr::null_mut(),
            surface_mailbox: apprt::surface::Mailbox {
                surface: std::ptr::null_mut(),
                app: crate::app::Mailbox {
                    rt_app: std::ptr::null(),
                    mailbox: std::ptr::null(),
                },
            },
            backend: backend::ThreadData::Exec(exec::ThreadData::default()),
            mailbox: std::ptr::null_mut(),
        }
    }
}

impl ThreadData {
    pub fn deinit(&mut self) {
        self.backend.deinit();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xt_checksum_flags_invert_config_names() {
        let flags = xt_checksum_flags(XtChecksumExtension::default());
        assert_eq!(
            xt_checksum::Flags {
                positive: false,
                no_attributes: false,
                no_trim: false,
                undrawn: false,
                full: false,
            },
            flags
        );

        let flags = xt_checksum_flags(XtChecksumExtension {
            negate: false,
            attributes: false,
            trim: false,
            undrawn: true,
            full: true,
        });
        assert_eq!(
            xt_checksum::Flags {
                positive: true,
                no_attributes: true,
                no_trim: true,
                undrawn: true,
                full: true,
            },
            flags
        );
    }

    #[test]
    fn derived_config_from_default_config() {
        let config = Config::default();
        let derived = DerivedConfig::init(&config);
        assert_eq!(config.palette.value, derived.palette);
        assert_eq!(config.image_storage_limit as usize, derived.image_storage_limit);
        assert_eq!(config.enquiry_response.as_bytes(), &derived.enquiry_response[..]);
        assert_eq!(config.clipboard_write_limit_bytes.value, derived.clipboard_write_limit);
    }

    #[test]
    fn derived_config_generates_palette() {
        let mut config = Config::default();
        config.palette_generate = true;

        // Nothing set manually: the default palette is used as-is.
        assert_eq!(config.palette.value, DerivedConfig::init(&config).palette);

        // A manual value triggers generation, which keeps the set index.
        config.palette.value[1] = terminal_color::Rgb { r: 1, g: 2, b: 3 };
        config.palette.mask.set(1);
        let derived = DerivedConfig::init(&config);
        assert_eq!(terminal_color::Rgb { r: 1, g: 2, b: 3 }, derived.palette[1]);
    }

    /// Tests that drive the stream handler through a real termio (with
    /// stand-in renderer and surface mailboxes; the IO thread is not
    /// started unless noted).
    #[cfg(unix)]
    mod handler {
        use super::*;
        use crate::apprt::surface::Message as SurfaceMessage;
        use crate::terminal::mouse;
        use crate::termio::exec::testing::{direct_config, Harness};

        fn harness() -> Harness {
            Harness::new(direct_config(&["/bin/sh", "-c", "exit 0"]))
        }

        /// The messages the stream handler queued for the IO thread.
        fn io_messages(h: &mut Harness) -> Vec<Message> {
            let Mailbox::Spsc { queue, .. } = &h.io.mailbox;
            let mut result = Vec::new();
            while let Some(m) = queue.pop() {
                result.push(m);
            }
            result
        }

        /// The bytes of the write messages queued for the pty.
        fn writes(h: &mut Harness) -> Vec<Vec<u8>> {
            io_messages(h)
                .into_iter()
                .filter_map(|m| match m {
                    Message::WriteSmall(v) => Some(v.data[..v.len as usize].to_vec()),
                    Message::WriteStable(v) => Some(v.to_vec()),
                    Message::WriteAlloc(v) => Some(v.to_vec()),
                    _ => None,
                })
                .collect()
        }

        fn surface_messages(h: &mut Harness) -> Vec<SurfaceMessage> {
            h.collect_messages();
            std::mem::take(&mut h.messages)
        }

        // The integration test: a real Exec, Thread and Termio running a
        // command on a pty, with the output parsed into the terminal.
        #[test]
        fn exec_thread_termio_printf_hello() {
            let mut h = Harness::new(direct_config(&["/bin/sh", "-c", "printf hello"]));
            h.start();
            h.wait_output("hello");
            h.wait_message(|m| matches!(m, SurfaceMessage::ChildExited(e) if e.exit_code == 0));
            h.stop();

            let screen = h.with_io(|io| io.terminal.plain_string().unwrap());
            assert_eq!("hello", screen.trim_end());
            assert_eq!(5, h.with_io(|io| io.terminal.screens.active.cursor.x));

            // The renderer was asked to reset the cursor blink.
            h.collect_renderer_messages();
            assert!(h
                .renderer_messages
                .iter()
                .any(|m| matches!(m, renderer::Message::ResetCursorBlink)));
        }

        #[test]
        fn print_and_cursor() {
            let mut h = harness();
            h.io.process_output(b"hello\r\nworld");
            assert_eq!("hello\nworld", h.io.terminal.plain_string().unwrap().trim_end());
            assert_eq!(1, h.io.terminal.screens.active.cursor.y);
            assert_eq!(5, h.io.terminal.screens.active.cursor.x);
            assert!(io_messages(&mut h).is_empty());
            assert!(!h.io.terminal_stream.handler.termio_messaged);
        }

        #[test]
        fn device_status_reports() {
            let mut h = harness();
            h.io.process_output(b"\x1b[3;5H\x1b[6n\x1b[?6n\x1b[5n");
            assert_eq!(
                vec![b"\x1b[3;5R".to_vec(), b"\x1b[?3;5;1R".to_vec(), b"\x1b[0n".to_vec()],
                writes(&mut h)
            );

            // Origin mode reports relative to the scrolling region.
            h.io.process_output(b"\x1b[2;10r\x1b[?6h\x1b[2;3H\x1b[6n");
            assert_eq!(vec![b"\x1b[2;3R".to_vec()], writes(&mut h));

            h.io.process_output(b"\x1b[?996n\x1b[?998n");
            let msgs = io_messages(&mut h);
            assert!(matches!(msgs[0], Message::ColorSchemeReport { force: true }));
            assert!(matches!(
                msgs[1],
                Message::VisibilityReport {
                    visible: true,
                    force: true
                }
            ));
        }

        #[test]
        fn device_attributes() {
            let mut h = harness();
            h.io.process_output(b"\x1b[c\x1b[>c");
            assert_eq!(
                vec![b"\x1b[?62;22;52c".to_vec(), b"\x1b[>1;10;0c".to_vec()],
                writes(&mut h)
            );

            h.io.terminal_stream.handler.clipboard_write = ClipboardAccess::Deny;
            h.io.process_output(b"\x1b[c");
            assert_eq!(vec![b"\x1b[?62;22c".to_vec()], writes(&mut h));
        }

        #[test]
        fn enquiry_xtversion_kitty_keyboard() {
            let mut h = harness();
            h.io.terminal_stream.handler.enquiry_response = b"answer".to_vec();
            h.io.process_output(b"\x05\x1b[>q\x1b[>1u\x1b[?u");
            let w = writes(&mut h);
            assert_eq!(b"answer".to_vec(), w[0]);
            assert_eq!(
                format!("\x1bP>|ghostty {}\x1b\\", crate::build_config::VERSION_STRING).into_bytes(),
                w[1]
            );
            assert_eq!(b"\x1b[?1u".to_vec(), w[2]);
            // processOutput woke the IO thread and cleared the flag.
            assert!(!h.io.terminal_stream.handler.termio_messaged);
        }

        #[test]
        fn mode_reports_and_side_effects() {
            let mut h = harness();
            h.io.process_output(b"\x1b[?2026$p\x1b[?2026h\x1b[?2026$p\x1b[?9999$p");
            let msgs = io_messages(&mut h);
            let w: Vec<Vec<u8>> = msgs
                .iter()
                .filter_map(|m| match m {
                    Message::WriteSmall(v) => Some(v.data[..v.len as usize].to_vec()),
                    _ => None,
                })
                .collect();
            assert_eq!(
                vec![
                    b"\x1b[?2026;2$y".to_vec(),
                    b"\x1b[?2026;1$y".to_vec(),
                    b"\x1b[?9999;0$y".to_vec()
                ],
                w
            );
            assert!(msgs.iter().any(|m| matches!(m, Message::StartSynchronizedOutput)));

            // Linefeed mode, size reports, focus events.
            h.io.process_output(b"\x1b[20h\x1b[?2048h\x1b[?1004h\x1b[14t");
            let msgs = io_messages(&mut h);
            assert!(matches!(msgs[0], Message::LinefeedMode(true)));
            assert!(matches!(msgs[1], Message::SizeReport(SizeReport::Mode2048)));
            assert!(matches!(msgs[2], Message::Focused(true)));
            assert!(matches!(msgs[3], Message::SizeReport(SizeReport::Csi14T)));

            // Mouse tracking changes the mouse shape once.
            h.io.process_output(b"\x1b[?1000h\x1b[?1006h");
            assert_eq!(mouse::Event::Normal, h.io.terminal.flags.mouse_event);
            assert_eq!(mouse::Format::Sgr, h.io.terminal.flags.mouse_format);
            h.io.process_output(b"\x1b[?1000l");
            let shapes: Vec<_> = surface_messages(&mut h)
                .into_iter()
                .filter_map(|m| match m {
                    SurfaceMessage::SetMouseShape(s) => Some(s),
                    _ => None,
                })
                .collect();
            assert_eq!(vec![mouse::Shape::Default, mouse::Shape::Text], shapes);

            // The alternate screen.
            h.io.process_output(b"\x1b[?1049h");
            assert_eq!(ScreenKey::Alternate, h.io.terminal.screens.active_key);
            h.io.process_output(b"\x1b[?1049l");
            assert_eq!(ScreenKey::Primary, h.io.terminal.screens.active_key);
        }

        #[test]
        fn cursor_blinking_ignored_with_configured_default() {
            let mut h = harness();
            h.io.terminal.cursor.default_blink = Some(true);
            let before = h.io.terminal.modes.get(Mode::CursorBlinking);
            h.io.process_output(if before { b"\x1b[?12l" } else { b"\x1b[?12h" });
            assert_eq!(before, h.io.terminal.modes.get(Mode::CursorBlinking));

            h.io.terminal.cursor.default_blink = None;
            h.io.process_output(b"\x1b[?12l");
            assert!(!h.io.terminal.modes.get(Mode::CursorBlinking));
        }

        #[test]
        fn bell_title_and_notifications() {
            let mut h = harness();
            h.io.process_output(b"\x07\x1b]2;my title\x1b\\\x1b]9;hi there\x1b\\");
            let msgs = surface_messages(&mut h);
            assert!(matches!(msgs[0], SurfaceMessage::RingBell));
            match &msgs[1] {
                SurfaceMessage::SetTitle(buf) => {
                    assert_eq!(b"my title\0", &buf[..9]);
                }
                other => panic!("unexpected {other:?}"),
            }
            assert!(matches!(&msgs[2], SurfaceMessage::DesktopNotification(_)));
            assert_eq!(Some(&b"my title"[..]), h.io.terminal.get_title());
            assert!(h.io.terminal_stream.handler.seen_title);

            // An empty title resets the title behavior.
            h.io.process_output(b"\x1b]2;\x1b\\");
            assert!(!h.io.terminal_stream.handler.seen_title);
            match &surface_messages(&mut h)[0] {
                SurfaceMessage::SetTitle(buf) => assert_eq!(0, buf[0]),
                other => panic!("unexpected {other:?}"),
            }
        }

        #[test]
        fn report_pwd() {
            let mut h = harness();
            h.io.process_output(b"\x1b]7;file://localhost/tmp/a%20b\x1b\\");
            assert_eq!(Some(&b"/tmp/a b"[..]), h.io.terminal.get_pwd());
            let msgs = surface_messages(&mut h);
            match &msgs[0] {
                SurfaceMessage::PwdChange(req) => {
                    let crate::datastruct::MessageData::Small(v) = req else {
                        panic!("unexpected {req:?}")
                    };
                    assert_eq!(b"/tmp/a b", &v.data[..v.len as usize]);
                }
                other => panic!("unexpected {other:?}"),
            }
            // Without a title the pwd becomes the title.
            match &msgs[1] {
                SurfaceMessage::SetTitle(buf) => assert_eq!(b"/tmp/a b\0", &buf[..9]),
                other => panic!("unexpected {other:?}"),
            }
            assert!(!h.io.terminal_stream.handler.seen_title);

            // A remote host is rejected.
            h.io.process_output(b"\x1b]7;file://not-this-host.invalid/etc\x1b\\");
            assert_eq!(Some(&b"/tmp/a b"[..]), h.io.terminal.get_pwd());
            assert!(surface_messages(&mut h).is_empty());

            // An empty URL resets the pwd.
            h.io.process_output(b"\x1b]7;\x1b\\");
            assert_eq!(None, h.io.terminal.get_pwd());
        }

        #[test]
        fn color_operations() {
            let mut h = harness();
            // Query the default foreground (16-bit report format).
            h.io.process_output(b"\x1b]10;?\x1b\\");
            assert_eq!(vec![b"\x1b]10;rgb:ffff/ffff/ffff\x1b\\".to_vec()], writes(&mut h));

            // Set a palette color and query it in 8-bit format.
            h.io.terminal_stream.handler.osc_color_report_format = OSCColorReportFormat::Bit8;
            h.io.process_output(b"\x1b]4;1;rgb:12/34/56\x07\x1b]4;1;?\x07");
            assert_eq!(vec![b"\x1b]4;1;rgb:12/34/56\x07".to_vec()], writes(&mut h));
            assert!(h.io.terminal.flags.dirty.palette);
            let msgs = surface_messages(&mut h);
            assert!(matches!(&msgs[0], SurfaceMessage::ColorChange(_)));

            // Reset the palette.
            h.io.process_output(b"\x1b]104\x07");
            assert_eq!(
                h.io.terminal.colors.palette.original()[1],
                h.io.terminal.colors.palette.current[1]
            );

            // Reporting disabled.
            h.io.terminal_stream.handler.osc_color_report_format = OSCColorReportFormat::None;
            h.io.process_output(b"\x1b]10;?\x1b\\");
            assert!(writes(&mut h).is_empty());
        }

        #[test]
        fn kitty_color_report() {
            let mut h = harness();
            h.io.process_output(b"\x1b]21;foreground=?;2=#ff0000;2=?\x1b\\");
            assert_eq!(
                vec![b"\x1b]21;foreground=rgb:ff/ff/ff;2=rgb:ff/00/00\x1b\\".to_vec()],
                writes(&mut h)
            );
        }

        #[test]
        fn dcs_xtgettcap_and_decrqss() {
            let mut h = harness();
            // TN = 544E
            h.io.process_output(b"\x1bP+q544E\x1b\\");
            let w = writes(&mut h);
            assert_eq!(1, w.len());
            assert!(w[0].starts_with(b"\x1bP1+r544E="), "{w:?}");

            h.io.process_output(b"\x1b[1m\x1bP$qm\x1b\\");
            assert_eq!(vec![b"\x1bP1$r0;1m\x1b\\".to_vec()], writes(&mut h));
        }

        #[test]
        fn clipboard_osc52() {
            let mut h = harness();
            h.io.process_output(b"\x1b]52;c;?\x07\x1b]52;p;aGVsbG8=\x07");
            let msgs = surface_messages(&mut h);
            assert!(matches!(msgs[0], SurfaceMessage::ClipboardRead(apprt::Clipboard::Standard)));
            match &msgs[1] {
                SurfaceMessage::ClipboardWrite { clipboard_type, req } => {
                    assert_eq!(apprt::Clipboard::Primary, *clipboard_type);
                    let crate::datastruct::MessageData::Small(v) = req else {
                        panic!("unexpected {req:?}")
                    };
                    assert_eq!(b"aGVsbG8=", &v.data[..v.len as usize]);
                }
                other => panic!("unexpected {other:?}"),
            }
        }

        #[test]
        fn kitty_clipboard_read_request() {
            let mut h = harness();
            // type=read with the text/plain MIME type (base64).
            h.io.process_output(b"\x1b]5522;type=read;dGV4dC9wbGFpbg==\x1b\\");
            match &surface_messages(&mut h)[0] {
                SurfaceMessage::KittyClipboardRead(req) => {
                    assert_eq!(1, req.mimes.len());
                    assert_eq!(b"text/plain", req.mimes[0].as_bytes());
                    assert!(!req.list);
                    assert!(!req.granted);
                }
                other => panic!("unexpected {other:?}"),
            }
        }

        #[test]
        fn kitty_clipboard_write_denied() {
            let mut h = harness();
            h.io.terminal_stream.handler.clipboard_write = ClipboardAccess::Deny;
            h.io.process_output(b"\x1b]5522;type=write:id=x\x1b\\");
            assert_eq!(
                vec![b"\x1b]5522;type=write:status=EPERM:id=x\x1b\\".to_vec()],
                writes(&mut h)
            );
            assert!(h.io.terminal_stream.handler.kitty_clipboard_write.is_none());
        }

        #[test]
        fn kitty_clipboard_write_commit() {
            let mut h = harness();
            // dGV4dC9wbGFpbg== is "text/plain", aGk= is "hi".
            h.io.process_output(
                b"\x1b]5522;type=write\x1b\\\x1b]5522;type=wdata:mime=dGV4dC9wbGFpbg==;aGk=\x1b\\\x1b]5522;type=wdata\x1b\\",
            );
            assert!(h.io.terminal_stream.handler.kitty_clipboard_write.is_none());
            match &surface_messages(&mut h)[0] {
                SurfaceMessage::KittyClipboardWrite(req) => {
                    assert_eq!(1, req.contents.len());
                    assert_eq!(b"text/plain", req.contents[0].mime.as_bytes());
                    assert_eq!(b"hi", &req.contents[0].data[..]);
                }
                other => panic!("unexpected {other:?}"),
            }
        }

        #[test]
        fn full_reset() {
            let mut h = harness();
            h.io.process_output(b"\x1b[?1000hhello\x1bc");
            assert_eq!("", h.io.terminal.plain_string().unwrap().trim_end());
            let msgs = io_messages(&mut h);
            assert!(msgs
                .iter()
                .any(|m| matches!(m, Message::ColorSchemeReport { force: false })));
            let surface = surface_messages(&mut h);
            assert!(surface.iter().any(|m| matches!(
                m,
                SurfaceMessage::ProgressReport(osc::ProgressReport {
                    state: osc::ProgressReportState::Remove,
                    ..
                })
            )));
        }

        #[test]
        fn semantic_prompt_commands() {
            let mut h = harness();
            h.io.process_output(b"\x1b]133;A\x07$ \x1b]133;B\x07ls\x1b]133;C\x07\r\n\x1b]133;D;3\x07");
            let msgs = surface_messages(&mut h);
            assert!(matches!(msgs[0], SurfaceMessage::StartCommand));
            assert!(matches!(msgs[1], SurfaceMessage::StopCommand(Some(3))));

            h.io.process_output(b"\x1b]133;D;300\x07");
            assert!(matches!(surface_messages(&mut h)[0], SurfaceMessage::StopCommand(Some(1))));
        }

        #[test]
        fn resize_window_and_title_report() {
            let mut h = harness();
            h.io.process_output(b"\x1b[8;30;100t\x1b[21t");
            let msgs = surface_messages(&mut h);
            match &msgs[0] {
                SurfaceMessage::ResizeWindow(v) => {
                    assert_eq!(30, v.rows);
                    assert_eq!(100, v.columns);
                }
                other => panic!("unexpected {other:?}"),
            }
            assert!(matches!(msgs[1], SurfaceMessage::ReportTitle(_)));
        }

        #[test]
        fn kitty_graphics_response() {
            let mut h = harness();
            // Query support for a 1x1 RGB image.
            h.io.process_output(b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\");
            assert_eq!(vec![b"\x1b_Gi=31;OK\x1b\\".to_vec()], writes(&mut h));
        }

        #[test]
        fn hyperlinks() {
            let mut h = harness();
            h.io.process_output(b"\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\");
            assert_eq!("link", h.io.terminal.plain_string().unwrap().trim_end());
        }

        #[test]
        fn clear_screen_not_at_prompt() {
            let mut h = harness();
            h.start();
            h.with_io(|io| io.process_output_locked(b"one\r\ntwo\r\nthree"));
            h.send(Message::ClearScreen { history: true });
            h.wait_io(|io| {
                let s = io.terminal.plain_string().unwrap();
                !s.contains("one") && !s.contains("two")
            });
            assert!(h.screen().contains("three"));
            h.stop();
        }

        #[test]
        fn termio_messages_on_the_thread() {
            let mut h = harness();
            h.start();

            h.send(Message::ScrollViewport(ScrollViewport::Top));
            h.send(Message::JumpToPrompt(1));
            h.send(Message::KittyClipboardGrantRead(crate::termio::message::KittyClipboardGrant {
                pw: b"pw".to_vec().into_boxed_slice(),
            }));
            h.wait_io(|io| {
                io.terminal_stream
                    .handler
                    .kitty_clipboard_grants
                    .clone()
                    .r#use(b"pw", KittyClipboardDirection::Read)
            });

            // A config change updates the handler and terminal defaults.
            let mut config = Config::default();
            config.enquiry_response = "changed".into();
            config.foreground = crate::config::config::Color { r: 1, g: 2, b: 3 };
            h.send(Message::ChangeConfig(Box::new(DerivedConfig::init(&config))));
            h.wait_io(|io| io.terminal_stream.handler.enquiry_response == b"changed");
            assert_eq!(
                Some(terminal_color::Rgb { r: 1, g: 2, b: 3 }),
                h.with_io(|io| io.terminal.colors.foreground.default)
            );
            h.stop();
        }
    }
}
