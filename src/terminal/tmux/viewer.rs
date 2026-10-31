//! Port of `src/terminal/tmux/viewer.zig`.
//!
//! STATUS: ported
//!
//! Notes on the port:
//! - `std.Io`/allocator parameters, the action arena and the pre-allocated
//!   single-action slot are dropped (allocation failure aborts in Rust).
//!   Actions produced by `next` are kept in the viewer until the next call,
//!   and `next` returns them as [`Actions`], which borrows the viewer and
//!   also gives read access to it (Zig returns a slice that points into the
//!   viewer while the caller keeps its `*Viewer`).
//! - `Window` owns its `Layout`; the per-window layout arena is dropped.
//! - `PanesMap` is an insertion-ordered `IndexMap` (Zig:
//!   `AutoArrayHashMapUnmanaged`). Panes that survive a layout sync are
//!   moved into the new map instead of being shallow-copied.
//! - `Pane.output_stream` is a boxed `TerminalStream<'static>` whose handler
//!   is re-pointed at the pane's terminal before every use, exactly like
//!   the Zig code does, because panes move when the map is rebuilt.
//! - `Format.Struct()` is spelled out with `format_struct!`.

// TODO: A list of TODOs as I think about them.
// - We need to make startup more robust so session and block can happen
//   out of order.
// - We need to ignore `output` for panes that aren't yet initialized
//   (until capture-panes are complete).
// - We should note what the active window pane is on the tmux side;
//   we can use this at least for initial focus.

// NOTE: There is some fragility here that can possibly break if tmux
// changes their implementation. In particular, the order of notifications
// and assurances about what is sent when are based on reading the tmux
// source code as of Dec, 2025. These aren't documented as fixed.
//
// I've tried not to depend on anything that seems like it'd change
// in the future. For example, it seems reasonable that command output
// always comes before session attachment. But, I am noting this here
// in case something breaks in the future we can consider it. We should
// be able to easily unit test all variations seen in the real world.

use std::fmt;
use std::io::Write as _;
use std::ops::Deref;

use indexmap::IndexMap;

use super::control::{self, Notification};
use super::layout::{self, Content, Layout};
use super::output::{self, format_struct, FormatStruct, Variable};
use crate::datastruct::circ_buf::CircBuf;
use crate::terminal::csi::EraseDisplay;
use crate::terminal::cursor::Style as CursorStyle;
use crate::terminal::modes::Mode;
use crate::terminal::screen_set::Key as ScreenKey;
use crate::terminal::size::{AllocError, CellCountInt};
use crate::terminal::stream_terminal::Stream as TerminalStream;
use crate::terminal::terminal::{InitError, Options as TerminalOptions, Terminal};

const LOG: &str = "terminal_tmux_viewer";

/// The initial capacity of the command queue. We dynamically resize
/// as necessary so the initial value isn't that important, but if we
/// want to feel good about it we should make it large enough to support
/// our most realistic use cases without resizing.
const COMMAND_QUEUE_INITIAL: usize = 8;

/// A viewer is a tmux control mode client that attempts to create
/// a remote view of a tmux session, including providing the ability to send
/// new input to the session.
///
/// This is the primary use case for tmux control mode, but technically
/// tmux control mode clients can do anything a normal tmux client can do,
/// so the `control.zig` and other files in this folder are more general
/// purpose.
///
/// This struct helps move through a state machine of connecting to a tmux
/// session, negotiating capabilities, listing window state, etc.
///
/// ## Viewer Lifecycle
///
/// The viewer progresses through several states from initial connection
/// to steady-state operation. Here is the full flow:
///
/// ```text
///                              ┌─────────────────────────────────────────────┐
///                              │           TMUX CONTROL MODE START           │
///                              │         (DCS 1000p received by host)        │
///                              └─────────────────┬───────────────────────────┘
///                                                │
///                                                ▼
///                              ┌─────────────────────────────────────────────┐
///                              │            startup_block                    │
///                              │                                             │
///                              │  Wait for initial %begin/%end block from    │
///                              │  tmux. This is the response to the initial  │
///                              │  command (e.g., "attach -t 0").             │
///                              └─────────────────┬───────────────────────────┘
///                                                │ %end / %error
///                                                ▼
///                              ┌─────────────────────────────────────────────┐
///                              │           startup_session                   │
///                              │                                             │
///                              │  Wait for %session-changed notification     │
///                              │  to get the initial session ID.             │
///                              └─────────────────┬───────────────────────────┘
///                                                │ %session-changed
///                                                ▼
///                              ┌─────────────────────────────────────────────┐
///                              │           command_queue                     │
///                              │                                             │
///                              │  Main operating state. Process commands     │
///                              │  sequentially and handle notifications.     │
///                              └─────────────────────────────────────────────┘
///                                                │
///                    ┌───────────────────────────┼───────────────────────────┐
///                    │                           │                           │
///                    ▼                           ▼                           ▼
///     ┌──────────────────────────┐ ┌──────────────────────────┐ ┌────────────────────────┐
///     │     tmux_version         │ │     list_windows         │ │   %output / %layout-   │
///     │                          │ │                          │ │   change / etc.        │
///     │  Query tmux version for  │ │  Get all windows in the  │ │                        │
///     │  compatibility checks.   │ │  current session.        │ │  Handle live updates   │
///     └──────────────────────────┘ └────────────┬─────────────┘ │  from tmux server.     │
///                                               │               └────────────────────────┘
///                                               ▼
///                              ┌─────────────────────────────────────────────┐
///                              │          syncLayouts                        │
///                              │                                             │
///                              │  For each window, parse layout and sync     │
///                              │  panes. New panes trigger capture commands. │
///                              └─────────────────┬───────────────────────────┘
///                                                │
///                    ┌───────────────────────────┴───────────────────────────┐
///                    │                  For each new pane:                   │
///                    ▼                                                       ▼
///     ┌──────────────────────────┐                            ┌──────────────────────────┐
///     │     pane_history         │                            │     pane_visible         │
///     │     (primary screen)     │                            │     (primary screen)     │
///     │                          │                            │                          │
///     │  Capture scrollback      │                            │  Capture visible area    │
///     │  history into terminal.  │                            │  into terminal.          │
///     └──────────────────────────┘                            └──────────────────────────┘
///                    │                                                       │
///                    ▼                                                       ▼
///     ┌──────────────────────────┐                            ┌──────────────────────────┐
///     │     pane_history         │                            │     pane_visible         │
///     │     (alternate screen)   │                            │     (alternate screen)   │
///     └──────────────────────────┘                            └──────────────────────────┘
///                    │                                                       │
///                    └───────────────────────────┬───────────────────────────┘
///                                                ▼
///                              ┌─────────────────────────────────────────────┐
///                              │          pane_state                         │
///                              │                                             │
///                              │  Query cursor position, cursor style,       │
///                              │  and alternate screen mode for all panes.   │
///                              └─────────────────────────────────────────────┘
///                                                │
///                                                ▼
///                              ┌─────────────────────────────────────────────┐
///                              │        READY FOR OPERATION                  │
///                              │                                             │
///                              │  Panes are populated with content. The      │
///                              │  viewer handles %output for live updates,   │
///                              │  %layout-change for pane changes, and       │
///                              │  %session-changed for session switches.     │
///                              └─────────────────────────────────────────────┘
/// ```
///
/// ## Error Handling
///
/// At any point, if an unrecoverable error occurs or tmux sends `%exit`,
/// the viewer transitions to the `defunct` state and emits an `.exit` action.
///
/// ## Session Changes
pub struct Viewer {
    /// Current state of the state machine.
    pub state: State,

    /// The current session ID we're attached to.
    pub session_id: usize,

    /// The tmux server version string (e.g., "3.5a"). We capture this
    /// on startup because it will allow us to change behavior between
    /// versions as necessary.
    pub tmux_version: Vec<u8>,

    /// The list of commands we've sent that we want to send and wait
    /// for a response for. We only send one command at a time just
    /// to avoid any possible confusion around ordering.
    pub command_queue: CommandQueue,

    /// The windows in the current session.
    pub windows: Vec<Window>,

    /// The panes in the current session, mapped by pane ID.
    pub panes: PanesMap,

    /// The actions produced by the prior `next` call (Zig: the action
    /// arena and the single-action slot). Cleared at the start of every
    /// `next` call.
    actions: Vec<StoredAction>,
}

pub type CommandQueue = CircBuf<Command>;
pub type PanesMap = IndexMap<usize, Pane>;

/// `Viewer.Action`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action<'a> {
    /// Tmux has closed the control mode connection, we should end
    /// our viewer session in some way.
    Exit,

    /// Send a command to tmux, e.g. `list-windows`. The caller
    /// should not worry about parsing this or reading what command
    /// it is; just send it to tmux as-is. This will include the
    /// trailing newline so you can send it directly.
    Command(&'a [u8]),

    /// Windows changed. This may add, remove or change windows. The
    /// caller is responsible for diffing the new window list against
    /// the prior one. Remember that for a given Viewer, window IDs
    /// are guaranteed to be stable. Additionally, tmux (as of Dec 2025)
    /// never reuses window IDs within a server process lifetime.
    Windows(&'a [Window]),
}

/// `std.meta.Tag(Viewer.Action)`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionTag {
    Exit,
    Command,
    Windows,
}

impl Action<'_> {
    pub fn tag(&self) -> ActionTag {
        match self {
            Action::Exit => ActionTag::Exit,
            Action::Command(_) => ActionTag::Command,
            Action::Windows(_) => ActionTag::Windows,
        }
    }
}

/// `Action.format`: `terminal.tmux.viewer.Viewer.Action{ .tag = value }`.
impl fmt::Display for Action<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "terminal.tmux.viewer.Viewer.Action{{ .")?;
        match self {
            Action::Exit => write!(f, "exit = void")?,
            Action::Command(v) => write!(f, "command = \"{}\"", String::from_utf8_lossy(trim(v, b" \t\r\n")))?,
            // Window embeds its layout; dumping it is noise.
            Action::Windows(v) => write!(f, "windows = [{} windows]", v.len())?,
        }
        write!(f, " }}")
    }
}

/// An action as stored in the viewer between `next` calls.
#[derive(Debug, Clone, PartialEq, Eq)]
enum StoredAction {
    Exit,
    Command(Vec<u8>),
    /// `windows` pointing at nothing (`&.{}`) or at `Viewer.windows`.
    Windows { empty: bool },
}

/// The actions returned by one `Viewer::next` call. Dereferences to the
/// action slice and keeps the viewer readable through `viewer`.
pub struct Actions<'a> {
    viewer: &'a Viewer,
    list: Vec<Action<'a>>,
}

impl<'a> Actions<'a> {
    /// The viewer that produced these actions.
    pub fn viewer(&self) -> &'a Viewer {
        self.viewer
    }
}

impl<'a> Deref for Actions<'a> {
    type Target = [Action<'a>];
    fn deref(&self) -> &[Action<'a>] {
        &self.list
    }
}

/// `Viewer.Input`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input<'a> {
    /// Data from tmux was received that needs to be processed.
    Tmux(Notification<'a>),
}

/// `Viewer.Window`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    pub id: usize,
    pub width: usize,
    pub height: usize,
    pub layout: Layout,
}

/// `Viewer.Pane`
pub struct Pane {
    pub terminal: Terminal,
    output_stream: Option<Box<TerminalStream<'static>>>,
}

impl Pane {
    pub fn new(terminal: Terminal) -> Pane {
        Pane {
            terminal,
            output_stream: None,
        }
    }

    /// The pane's terminal as a lifetime-erased reference for the
    /// persistent output stream's handler.
    fn terminal_static(&mut self) -> &'static mut Terminal {
        // SAFETY: the stream is owned by this pane and dropped before the
        // terminal (see `Drop`). Its handler reference is re-pointed here
        // before every use, so it never uses a stale address after the pane
        // moved, and it is not used while other references to the terminal
        // are live.
        unsafe { &mut *(&mut self.terminal as *mut Terminal) }
    }

    fn output_stream(&mut self) -> &mut TerminalStream<'static> {
        if self.output_stream.is_none() {
            let t = self.terminal_static();
            self.output_stream = Some(Box::new(t.vt_stream()));
        }

        // Pane values can move when the pane map is rebuilt.
        let t = self.terminal_static();
        let stream = self.output_stream.as_mut().unwrap();
        stream.handler.terminal = t;
        stream
    }
}

impl Drop for Pane {
    fn drop(&mut self) {
        if let Some(mut stream) = self.output_stream.take() {
            // The pane may have moved since its last output.
            stream.handler.terminal = self.terminal_static();
            stream.deinit();
        }
    }
}

/// Errors from processing tmux input. Any of them moves the viewer to the
/// defunct state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    OutOfMemory,
    InvalidValue,
    InvalidLayout,
    Layout(layout::ParseError),
    Output(output::ParseError),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Layout(e) => write!(f, "{e}"),
            Error::Output(e) => write!(f, "{e}"),
            other => write!(f, "{other:?}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<AllocError> for Error {
    fn from(_: AllocError) -> Self {
        Error::OutOfMemory
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

impl From<layout::ParseError> for Error {
    fn from(e: layout::ParseError) -> Self {
        Error::Layout(e)
    }
}

impl From<output::ParseError> for Error {
    fn from(e: output::ParseError) -> Self {
        Error::Output(e)
    }
}

/// `std.mem.trim`
fn trim<'a>(s: &'a [u8], values: &[u8]) -> &'a [u8] {
    let start = s.iter().position(|b| !values.contains(b)).unwrap_or(s.len());
    let end = s.iter().rposition(|b| !values.contains(b)).map_or(start, |i| i + 1);
    &s[start..end.max(start)]
}

impl Default for Viewer {
    fn default() -> Self {
        Viewer::init()
    }
}

impl Viewer {
    /// Initialize a new viewer.
    pub fn init() -> Viewer {
        Viewer {
            state: State::StartupBlock,
            // The default value here is meaningless. We don't get started
            // until we receive a session-changed notification which will
            // set this to a real value.
            session_id: 0,
            tmux_version: Vec::new(),
            command_queue: CircBuf::init(COMMAND_QUEUE_INITIAL, Command::ListWindows),
            windows: Vec::new(),
            panes: PanesMap::new(),
            actions: Vec::new(),
        }
    }

    /// Send in an input event (such as a tmux protocol notification,
    /// keyboard input for a pane, etc.) and process it. The returned
    /// list is a set of actions to take as a result of the input prior
    /// to the next input. This list may be empty.
    pub fn next(&mut self, input: Input<'_>) -> Actions<'_> {
        // Developer note: this function must never return an error. If
        // an error occurs we must go into a defunct state or some other
        // state to gracefully handle it.
        self.actions.clear();
        match input {
            Input::Tmux(n) => self.next_tmux(n),
        }

        let this: &Viewer = self;
        let list = this
            .actions
            .iter()
            .map(|action| match action {
                StoredAction::Exit => Action::Exit,
                StoredAction::Command(v) => Action::Command(v),
                StoredAction::Windows { empty: true } => Action::Windows(&[]),
                StoredAction::Windows { empty: false } => Action::Windows(&this.windows),
            })
            .collect();
        Actions { viewer: this, list }
    }

    fn next_tmux(&mut self, n: Notification<'_>) {
        match self.state {
            State::Defunct => {
                log::info!(target: LOG, "received notification in defunct state, ignoring");
            }
            State::StartupBlock => self.next_startup_block(n),
            State::StartupSession => self.next_startup_session(n),
            State::CommandQueue => self.next_command(n),
        }
    }

    fn next_startup_block(&mut self, n: Notification<'_>) {
        debug_assert!(self.state == State::StartupBlock);

        match n {
            // This is only sent by the DCS parser when we first get
            // DCS 1000p, it should never reach us here.
            Notification::Enter => unreachable!(),

            // I don't think this is technically possible (reading the
            // tmux source code), but if we see an exit we can semantically
            // handle this without issue.
            Notification::Exit => self.defunct(),

            // Any begin and end (even error) is fine! Now we wait for
            // session-changed to get the initial session ID. session-changed
            // is guaranteed to come after the initial command output
            // since if the initial command is `attach` tmux will run that,
            // queue the notification, then do notificatins.
            Notification::BlockEnd(_) | Notification::BlockErr(_) => {
                self.state = State::StartupSession;
            }

            // I don't like catch-all else branches but startup is such
            // a special case of looking for very specific things that
            // are unlikely to expand.
            _ => {}
        }
    }

    fn next_startup_session(&mut self, n: Notification<'_>) {
        debug_assert!(self.state == State::StartupSession);

        match n {
            Notification::Enter => unreachable!(),
            Notification::Exit => self.defunct(),
            Notification::SessionChanged { id, .. } => {
                self.session_id = id;
                self.enter_command_queue(&[Command::TmuxVersion, Command::ListWindows]);
            }
            _ => {}
        }
    }

    fn next_command(&mut self, n: Notification<'_>) {
        // We have to be in a command queue, but the command queue MAY
        // be empty. If it is empty, then receivedCommandOutput will
        // handle it by ignoring any command output. That's okay!
        debug_assert!(self.state == State::CommandQueue);

        // Track whether the in-flight command slot is available. Starts true
        // if queue is empty (no command in flight). Set to true when a command
        // completes (block_end/block_err) or the queue is reset (session_changed).
        let mut command_consumed = self.command_queue.empty();

        match n {
            Notification::Enter => unreachable!(),
            Notification::Exit => return self.defunct(),

            Notification::BlockEnd(content) | Notification::BlockErr(content) => {
                let is_err = matches!(n, Notification::BlockErr(_));
                if self.received_command_output(content, is_err).is_err() {
                    log::warn!(target: LOG, "failed to process command output, becoming defunct");
                    return self.defunct();
                }

                // Command is consumed since a block end/err is the output
                // from a command.
                command_consumed = true;
            }

            Notification::Output { pane_id, data } => {
                if let Err(err) = self.received_output(pane_id, data) {
                    log::warn!(target: LOG, "failed to process output for pane id={pane_id}: {err}");
                }
            }

            // Session changed means we switched to a different tmux session.
            // We need to reset our state and start fresh with list-windows.
            // This completely replaces the viewer, so treat it like a fresh start.
            Notification::SessionChanged { id, .. } => {
                self.session_changed(id);

                // Command is consumed because sessionChanged resets
                // our entire viewer.
                command_consumed = true;
            }

            // Layout changed of a single window.
            Notification::LayoutChange { window_id, layout, .. } => {
                if self.layout_changed(window_id, layout).is_err() {
                    // Note: in the future, we can probably handle a failure
                    // here with a fallback to remove this one window, list
                    // windows again, and try again.
                    log::warn!(target: LOG, "failed to handle layout change, becoming defunct");
                    return self.defunct();
                }
            }

            // A window was added to this session.
            Notification::WindowAdd { id } => self.window_add(id),

            // The active pane changed. We don't care about this because
            // we handle our own focus.
            Notification::WindowPaneChanged { .. } => {}

            // We ignore this one. It means a session was created or
            // destroyed. If it was our own session we will get an exit
            // notification very soon. If it is another session we don't
            // care.
            Notification::SessionsChanged => {}

            // We don't use window names for anything, currently.
            Notification::WindowRenamed { .. } => {}

            // This is for other clients, which we don't do anything about.
            // For us, we'll get `exit` or `session_changed`, respectively.
            Notification::ClientDetached { .. } | Notification::ClientSessionChanged { .. } => {}
        }

        // After processing commands, we add our next command to
        // execute if we have one. We do this last because command
        // processing may itself queue more commands. We only emit a
        // command if a prior command was consumed (or never existed).
        if self.state == State::CommandQueue && command_consumed {
            if let Some(next_command) = self.command_queue.first() {
                // We should not have any commands, because our nextCommand
                // always queues them.
                debug_assert!(!self
                    .actions
                    .iter()
                    .any(|action| matches!(action, StoredAction::Command(_))));

                let mut builder: Vec<u8> = Vec::new();
                next_command.format_command(&mut builder);
                self.actions.push(StoredAction::Command(builder));
            }
        }
    }

    /// When the layout changes for a single window, a pane may be added
    /// or removed that we've never seen, in addition to the layout itself
    /// physically changing.
    ///
    /// To handle this, its similar to list-windows except we expect the
    /// window to already exist. We update the layout, do the initLayout
    /// call for any diffs, setup commands to capture any new panes,
    /// prune any removed panes.
    fn layout_changed(&mut self, window_id: usize, layout_str: &[u8]) -> Result<(), Error> {
        // Find the window this layout change is for.
        let Some(window) = self.windows.iter_mut().find(|w| w.id == window_id) else {
            log::info!(target: LOG, "layout change for unknown window id={window_id}");
            return Ok(());
        };

        // Setup our layout
        window.layout = Layout::parse_with_checksum(layout_str).inspect_err(|_| {
            log::info!(
                target: LOG,
                "failed to parse window layout id={} layout={}",
                window_id,
                String::from_utf8_lossy(layout_str)
            );
        })?;

        // Our initial action is to definitely let the caller know that
        // some windows changed.
        self.actions.push(StoredAction::Windows { empty: false });

        // Sync up our panes
        self.sync_layouts(None)
    }

    /// When a window is added to the session, we need to refresh our window
    /// list to get the new window's information.
    fn window_add(&mut self, _window_id: usize) {
        // We refresh all windows via list-windows.
        // Queue list-windows to get the updated window list
        self.queue_commands(&[Command::ListWindows]);
    }

    /// Sync the panes with the layouts of `windows`, or of the current
    /// windows when null (Zig: `windows.ptr == self.windows.items.ptr`). A
    /// new window list replaces the current one on success.
    fn sync_layouts(&mut self, windows: Option<Vec<Window>>) -> Result<(), Error> {
        // Go through the window layout and setup all our panes. We move
        // this into a new panes map so that we can easily prune our old
        // list.
        let mut panes = PanesMap::new();

        // Panes moved over from `self.panes` (Zig copies them and checks
        // `self.panes.contains`).
        let mut existing: Vec<usize> = Vec::new();
        {
            let layouts = windows.as_deref().unwrap_or(&self.windows);
            for window in layouts {
                if let Err(err) = init_layout(&mut self.panes, &mut panes, &mut existing, &window.layout) {
                    // Return the moved panes and drop the new ones.
                    for id in existing {
                        if let Some(pane) = panes.swap_remove(&id) {
                            self.panes.insert(id, pane);
                        }
                    }
                    return Err(err);
                }
            }
        }

        // Get our list of added panes and setup our command queue
        // to populate them.
        {
            let mut added = false;
            let ids: Vec<usize> = panes.keys().copied().filter(|id| !existing.contains(id)).collect();
            for pane_id in ids {
                added = true;
                self.queue_commands(&[
                    Command::PaneHistory(CapturePane {
                        id: pane_id,
                        screen_key: ScreenKey::Primary,
                    }),
                    Command::PaneVisible(CapturePane {
                        id: pane_id,
                        screen_key: ScreenKey::Primary,
                    }),
                    Command::PaneHistory(CapturePane {
                        id: pane_id,
                        screen_key: ScreenKey::Alternate,
                    }),
                    Command::PaneVisible(CapturePane {
                        id: pane_id,
                        screen_key: ScreenKey::Alternate,
                    }),
                ]);
            }

            // If we added any panes, then we also want to resync the pane
            // state (terminal modes and cursor positions and so on).
            if added {
                self.queue_commands(&[Command::PaneState]);
            }
        }

        // Replace our window list if it changed.
        if let Some(windows) = windows {
            self.windows = windows;
        }

        // Replace our panes. The panes left in the old map were removed.
        self.panes = panes;
        Ok(())
    }

    /// When a session changes, we have to basically reset our whole state.
    /// To do this, we emit an empty windows event (so callers can clear all
    /// windows), reset ourself, and start all over.
    fn session_changed(&mut self, session_id: usize) {
        // Build up a new viewer. Its the easiest way to reset ourselves.
        let mut replacement = Viewer::init();

        // Our actions must start out empty so we don't mix arenas
        debug_assert!(self.actions.is_empty());

        // Build actions: empty windows notification + list-windows command
        replacement.actions.push(StoredAction::Windows { empty: true });

        // Setup our command queue and put ourselves in the command queue
        // state.
        replacement.queue_commands(&[Command::ListWindows]);
        replacement.state = State::CommandQueue;

        // Transfer preserved version to replacement
        replacement.tmux_version = self.tmux_version.clone();

        // Swap our self.
        *self = replacement;

        // Set our session ID and jump directly to the list
        self.session_id = session_id;

        debug_assert!(self.state == State::CommandQueue);
    }

    fn received_command_output(&mut self, content: &[u8], is_err: bool) -> Result<(), Error> {
        // Get the command we're expecting output for.
        let Some(command) = self.command_queue.first().cloned() else {
            // If we have no pending commands, this is unexpected.
            log::info!(target: LOG, "unexpected block output err={is_err}");
            return Ok(());
        };
        self.command_queue.delete_oldest(1);

        // Process our command
        match command {
            Command::User(_) => Ok(()),
            Command::PaneState => self.received_pane_state(content),
            Command::ListWindows => self.received_list_windows(content),
            Command::PaneHistory(cap) => self.received_pane_history(cap.screen_key, cap.id, content),
            Command::PaneVisible(cap) => self.received_pane_visible(cap.screen_key, cap.id, content),
            Command::TmuxVersion => self.received_tmux_version(content),
        }
    }

    fn received_tmux_version(&mut self, content: &[u8]) -> Result<(), Error> {
        let line = trim(content, b" \t\r\n");
        if line.is_empty() {
            return Ok(());
        }

        let data: TmuxVersionFormat<'_> =
            output::parse_format_struct(line, Format::TMUX_VERSION.delim).inspect_err(|_| {
                log::info!(target: LOG, "failed to parse tmux version: {}", String::from_utf8_lossy(line));
            })?;

        self.tmux_version = data.version.to_vec();
        Ok(())
    }

    fn received_list_windows(&mut self, content: &[u8]) -> Result<(), Error> {
        // If there is an error, reset our actions to what it was before.
        let actions_len = self.actions.len();
        let result = (|| -> Result<(), Error> {
            // This stores our new window state from this list-windows output.
            let mut windows: Vec<Window> = Vec::new();

            // Parse all our windows
            for line_raw in content.split(|&c| c == b'\n') {
                let line = trim(line_raw, b" \t\r");
                if line.is_empty() {
                    continue;
                }
                let data: ListWindowsFormat<'_> = output::parse_format_struct(line, Format::LIST_WINDOWS.delim)
                    .inspect_err(|_| {
                        log::info!(
                            target: LOG,
                            "failed to parse list-windows line: {}",
                            String::from_utf8_lossy(line)
                        );
                    })?;

                // Parse the layout
                let layout = Layout::parse_with_checksum(data.window_layout).inspect_err(|_| {
                    log::info!(
                        target: LOG,
                        "failed to parse window layout id={} layout={}",
                        data.window_id,
                        String::from_utf8_lossy(data.window_layout)
                    );
                })?;

                windows.push(Window {
                    id: data.window_id,
                    width: data.window_width,
                    height: data.window_height,
                    layout,
                });
            }

            // Sync into self.windows first. The `.windows` action must point
            // at `self.windows`, not the temporary list.
            self.sync_layouts(Some(windows))?;
            self.actions.push(StoredAction::Windows { empty: false });
            Ok(())
        })();
        if result.is_err() {
            self.actions.truncate(actions_len);
        }
        result
    }

    fn received_pane_state(&mut self, content: &[u8]) -> Result<(), Error> {
        for line_raw in content.split(|&c| c == b'\n') {
            let line = trim(line_raw, b" \t\r");
            if line.is_empty() {
                continue;
            }

            let data: ListPanesFormat<'_> =
                output::parse_format_struct(line, Format::LIST_PANES.delim).inspect_err(|_| {
                    log::info!(target: LOG, "failed to parse list-panes line: {}", String::from_utf8_lossy(line));
                })?;

            // Get the pane for this ID
            let Some(pane) = self.panes.get_mut(&data.pane_id) else {
                log::info!(target: LOG, "received pane state for untracked pane id={}", data.pane_id);
                continue;
            };
            let t: &mut Terminal = &mut pane.terminal;

            // Determine which screen to use based on alternate_on
            let screen_key = if data.alternate_on {
                ScreenKey::Alternate
            } else {
                ScreenKey::Primary
            };

            // Restore the active screen reported by tmux.
            t.switch_screen(screen_key)?;

            // Set cursor position on the appropriate screen (tmux uses 0-based)
            if let Some(mut screen) = t.screens.get(screen_key) {
                'cursor: {
                    let Ok(cursor_x) = CellCountInt::try_from(data.cursor_x) else {
                        break 'cursor;
                    };
                    let Ok(cursor_y) = CellCountInt::try_from(data.cursor_y) else {
                        break 'cursor;
                    };
                    if cursor_x >= screen.pages.cols || cursor_y >= screen.pages.rows {
                        break 'cursor;
                    }
                    screen.cursor_absolute(cursor_x, cursor_y);
                }

                // Set cursor shape on this screen
                match data.cursor_shape {
                    b"block" => screen.cursor.cursor_style = CursorStyle::Block,
                    b"underline" => screen.cursor.cursor_style = CursorStyle::Underline,
                    b"bar" => screen.cursor.cursor_style = CursorStyle::Bar,
                    // "default" or unknown: leave as-is
                    _ => {}
                }
            }

            // Set alternate screen saved cursor position
            if let Some(mut alt_screen) = t.screens.get(ScreenKey::Alternate) {
                'cursor: {
                    let Ok(alt_x) = CellCountInt::try_from(data.alternate_saved_x) else {
                        break 'cursor;
                    };
                    let Ok(alt_y) = CellCountInt::try_from(data.alternate_saved_y) else {
                        break 'cursor;
                    };

                    // If our coordinates are outside our screen we ignore it.
                    // tmux actually sends MAX_INT for when there isn't a set
                    // cursor position, so this isn't theoretical.
                    if alt_x >= alt_screen.pages.cols || alt_y >= alt_screen.pages.rows {
                        break 'cursor;
                    }

                    alt_screen.cursor_absolute(alt_x, alt_y);
                }
            }

            // Set cursor visibility
            t.modes.set(Mode::CursorVisible, data.cursor_flag);

            // Set cursor blinking
            t.modes.set(Mode::CursorBlinking, data.cursor_blinking);

            // Terminal modes
            t.modes.set(Mode::Insert, data.insert_flag);
            t.modes.set(Mode::Wraparound, data.wrap_flag);
            t.modes.set(Mode::KeypadKeys, data.keypad_flag);
            t.modes.set(Mode::CursorKeys, data.keypad_cursor_flag);
            t.modes.set(Mode::Origin, data.origin_flag);

            // Mouse modes
            t.modes.set(Mode::MouseEventAny, data.mouse_all_flag);
            t.modes.set(Mode::MouseEventButton, data.mouse_button_flag);
            t.modes.set(Mode::MouseEventNormal, data.mouse_standard_flag);
            t.modes.set(Mode::MouseFormatUtf8, data.mouse_utf8_flag);
            t.modes.set(Mode::MouseFormatSgr, data.mouse_sgr_flag);

            // Focus and bracketed paste
            t.modes.set(Mode::FocusEvent, data.focus_flag);
            t.modes.set(Mode::BracketedPaste, data.bracketed_paste);

            // Scroll region (tmux uses 0-based values)
            'scroll: {
                let Ok(scroll_top) = CellCountInt::try_from(data.scroll_region_upper) else {
                    break 'scroll;
                };
                let Ok(scroll_bottom) = CellCountInt::try_from(data.scroll_region_lower) else {
                    break 'scroll;
                };

                // Ignore invalid regions. The terminal assumes the scroll
                // region is always within the screen bounds.
                if scroll_top >= scroll_bottom || scroll_bottom >= t.rows {
                    break 'scroll;
                }

                t.scrolling_region.top = scroll_top;
                t.scrolling_region.bottom = scroll_bottom;
            }

            // Tab stops - parse comma-separated list and set
            t.tabstops.reset(0); // Clear all tabstops first
            if !data.pane_tabs.is_empty() {
                for tab_str in data.pane_tabs.split(|&c| c == b',') {
                    let Ok(col) = output::parse_int_usize(tab_str) else {
                        continue;
                    };
                    let Ok(col_cell) = CellCountInt::try_from(col) else {
                        continue;
                    };
                    if col_cell >= t.cols {
                        continue;
                    }
                    t.tabstops.set(col_cell as usize);
                }
            }
        }
        Ok(())
    }

    fn received_pane_history(&mut self, screen_key: ScreenKey, id: usize, content: &[u8]) -> Result<(), Error> {
        // Get our pane
        let Some(pane) = self.panes.get_mut(&id) else {
            log::info!(target: LOG, "received pane history for untracked pane id={id}");
            return Ok(());
        };
        let t: &mut Terminal = &mut pane.terminal;
        t.switch_screen(screen_key)?;

        // Get a VT stream from the terminal so we can send data as-is into
        // it. This will populate the active area too so it won't be exactly
        // correct but we'll get the active contents soon.
        {
            let mut stream = t.vt_stream();
            stream.next_slice(content);
            stream.deinit();
        }

        // Populate the active area to be empty since this is only history.
        // We'll fill it with blanks and move the cursor to the top-left.
        t.carriage_return();
        for _ in 0..t.rows {
            t.index()?;
        }
        t.set_cursor_pos(1, 1);

        // Our active area should be empty
        if cfg!(debug_assertions) {
            let screen = &*t.screens.active;
            let mut discarding: Vec<u8> = Vec::new();
            screen
                .dump_string(
                    &mut discarding,
                    crate::terminal::screen::DumpString {
                        tl: screen.pages.get_top_left(crate::terminal::point::Tag::Active),
                        br: None,
                        unwrap: false,
                    },
                )
                .unwrap();
            debug_assert!(discarding.is_empty());
        }
        Ok(())
    }

    fn received_pane_visible(&mut self, screen_key: ScreenKey, id: usize, content: &[u8]) -> Result<(), Error> {
        // Get our pane
        let Some(pane) = self.panes.get_mut(&id) else {
            log::info!(target: LOG, "received pane visible for untracked pane id={id}");
            return Ok(());
        };
        let t: &mut Terminal = &mut pane.terminal;
        t.switch_screen(screen_key)?;

        // Erase the active area and reset the cursor to the top-left
        // before writing the visible content.
        t.erase_display(EraseDisplay::Complete, false);
        t.set_cursor_pos(1, 1);

        let mut stream = t.vt_stream();
        stream.next_slice(content);
        stream.deinit();
        Ok(())
    }

    fn received_output(&mut self, id: usize, data: &[u8]) -> Result<(), Error> {
        let Some(pane) = self.panes.get_mut(&id) else {
            log::info!(target: LOG, "received output for untracked pane id={id}");
            return Ok(());
        };
        let stream = pane.output_stream();
        stream.next_slice(data);
        Ok(())
    }

    /// Enters the command queue state from any other state, queueing
    /// the commands and returning an action to execute the first command.
    fn enter_command_queue(&mut self, commands: &[Command]) {
        debug_assert!(self.state != State::CommandQueue);
        debug_assert!(!commands.is_empty());

        // Build our command string to send for the action.
        let mut builder: Vec<u8> = Vec::new();
        commands[0].format_command(&mut builder);

        // Add our commands
        self.queue_commands(commands);

        // Move into the command queue state
        self.state = State::CommandQueue;

        self.single_action(StoredAction::Command(builder));
    }

    /// Queue multiple commands to execute. This doesn't add anything
    /// to the actions queue or return actions or anything because the
    /// command_queue state will automatically send the next command when
    /// it receives output.
    fn queue_commands(&mut self, commands: &[Command]) {
        self.command_queue.ensure_unused_capacity(commands.len());
        for command in commands {
            self.command_queue.append_assume_capacity(command.clone());
        }
    }

    /// Helper to return a single action.
    fn single_action(&mut self, action: StoredAction) {
        self.actions.clear();
        self.actions.push(action);
    }

    fn defunct(&mut self) {
        self.state = State::Defunct;
        self.single_action(StoredAction::Exit);
    }
}

/// Walk `layout` and make sure every pane in it has an entry in
/// `panes_new`.
///
/// Panes that already exist in `panes_old` are moved over as is (their IDs
/// are recorded in `existing`). New panes get a fresh terminal sized to
/// match the layout.
///
/// The layout comes from the control mode stream, which any program
/// running in the terminal can write to. Returns `InvalidLayout`
/// if a new pane's size is zero or too large for a terminal. Callers
/// treat any error as fatal and move the viewer to the defunct state.
fn init_layout(
    panes_old: &mut PanesMap,
    panes_new: &mut PanesMap,
    existing: &mut Vec<usize>,
    layout: &Layout,
) -> Result<(), Error> {
    match &layout.content {
        // Nested layouts, continue going.
        Content::Horizontal(layouts) | Content::Vertical(layouts) => {
            for l in layouts {
                init_layout(panes_old, panes_new, existing, l)?;
            }
        }

        // A leaf! Initialize.
        Content::Pane(id) => {
            let id = *id;
            if panes_new.contains_key(&id) {
                return Ok(());
            }

            // If we already have this pane, it is already initialized
            // so just move it over.
            if let Some(pane) = panes_old.swap_remove(&id) {
                panes_new.insert(id, pane);
                existing.push(id);
                return Ok(());
            }

            // The size comes from untrusted input. Make sure it fits
            // in a terminal before we create one.
            let cols = CellCountInt::try_from(layout.width).map_err(|_| Error::InvalidLayout)?;
            let rows = CellCountInt::try_from(layout.height).map_err(|_| Error::InvalidLayout)?;
            if cols == 0 || rows == 0 {
                return Err(Error::InvalidLayout);
            }

            let t = Terminal::init(TerminalOptions::new(cols, rows))?;
            panes_new.insert(id, Pane::new(t));
        }
    }
    Ok(())
}

/// `Viewer.State` (Zig: the file-level `State`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// We start in this state just after receiving the initial
    /// DCS 1000p opening sequence. We wait for an initial
    /// begin/end block that is guaranteed to be sent by tmux for
    /// the initial control mode command. (See tmux server-client.c
    /// where control mode starts).
    StartupBlock,

    /// After receiving the initial block, we wait for a session-changed
    /// notification to record the initial session ID.
    StartupSession,

    /// Tmux has closed the control mode connection
    Defunct,

    /// We're sitting on the command queue waiting for command output
    /// in the order provided in the `command_queue` field. This field
    /// isn't part of the state because it can be queued at any state.
    ///
    /// Precondition: if self.command_queue.len > 0, then the first
    /// command in the queue has already been sent to tmux (via a
    /// `command` Action). The next output is assumed to be the result
    /// of this command.
    ///
    /// To satisfy the above, any transitions INTO this state should
    /// send a command Action for the first command in the queue.
    CommandQueue,
}

/// `Command.CapturePane`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapturePane {
    pub id: usize,
    pub screen_key: ScreenKey,
}

/// A command sent to tmux whose output the viewer waits for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// List all windows so we can sync our window state.
    ListWindows,

    /// Capture history for the given pane ID.
    PaneHistory(CapturePane),

    /// Capture visible area for the given pane ID.
    PaneVisible(CapturePane),

    /// Capture the pane terminal state as best we can. The pane ID(s)
    /// are part of the output so we can map it back to our panes.
    PaneState,

    /// Get the tmux server version.
    TmuxVersion,

    /// User command. This is a command provided by the user. Since
    /// this is user provided, we can't be sure what it is.
    User(Vec<u8>),
}

impl Command {
    /// Format the command into the command that should be executed
    /// by tmux. Trailing newlines are appended so this can be sent as-is
    /// to tmux.
    pub fn format_command(&self, writer: &mut Vec<u8>) {
        // Writing to a Vec cannot fail.
        let _ = match self {
            Command::ListWindows => writeln!(writer, "list-windows -F '{}'", Format::LIST_WINDOWS.comptime_format()),

            Command::PaneHistory(cap) => writeln!(
                writer,
                // -p = output to stdout instead of buffer
                // -e = output escape sequences for SGR
                // -a = capture alternate screen (only valid for alternate)
                // -q = quiet, don't error if alternate screen doesn't exist
                // -S - = start at the top of history ("-")
                // -E -1 = end at the last line of history (1 before the
                //   visible area is -1).
                // -t %{d} = target a specific pane ID
                "capture-pane -p -e -q {}-S - -E -1 -t %{}",
                if cap.screen_key == ScreenKey::Alternate { "-a " } else { "" },
                cap.id,
            ),

            Command::PaneVisible(cap) => writeln!(
                writer,
                // -p = output to stdout instead of buffer
                // -e = output escape sequences for SGR
                // -a = capture alternate screen (only valid for alternate)
                // -q = quiet, don't error if alternate screen doesn't exist
                // -t %{d} = target a specific pane ID
                // (no -S/-E = capture visible area only)
                "capture-pane -p -e -q {}-t %{}",
                if cap.screen_key == ScreenKey::Alternate { "-a " } else { "" },
                cap.id,
            ),

            Command::PaneState => writeln!(writer, "list-panes -F '{}'", Format::LIST_PANES.comptime_format()),

            Command::TmuxVersion => writeln!(writer, "display-message -p '{}'", Format::TMUX_VERSION.comptime_format()),

            Command::User(v) => writer.write_all(v),
        };
    }
}

/// Format strings used for commands in our viewer.
struct Format {
    /// The variables included in this format, in order.
    vars: &'static [Variable],

    /// The delimiter to use between variables. This must be a character
    /// guaranteed to not appear in any of the variable outputs.
    delim: u8,
}

impl Format {
    const LIST_PANES: Format = Format {
        delim: b';',
        vars: <ListPanesFormat<'static> as FormatStruct<'static>>::VARS,
    };

    const LIST_WINDOWS: Format = Format {
        delim: b' ',
        vars: <ListWindowsFormat<'static> as FormatStruct<'static>>::VARS,
    };

    const TMUX_VERSION: Format = Format {
        delim: b' ',
        vars: <TmuxVersionFormat<'static> as FormatStruct<'static>>::VARS,
    };

    /// The format string (Zig: available at comptime).
    fn comptime_format(&self) -> String {
        output::comptime_format(self.vars, self.delim)
    }
}

format_struct!(
    /// `Format.list_panes.Struct()`
    struct ListPanesFormat {
        pane_id,
        // Cursor position & appearance
        cursor_x,
        cursor_y,
        cursor_flag,
        cursor_shape,
        cursor_colour,
        cursor_blinking,
        // Alternate screen
        alternate_on,
        alternate_saved_x,
        alternate_saved_y,
        // Terminal modes
        insert_flag,
        wrap_flag,
        keypad_flag,
        keypad_cursor_flag,
        origin_flag,
        // Mouse modes
        mouse_all_flag,
        mouse_button_flag,
        mouse_standard_flag,
        mouse_utf8_flag,
        mouse_sgr_flag,
        // Focus & special features
        focus_flag,
        bracketed_paste,
        // Scroll region
        scroll_region_upper,
        scroll_region_lower,
        // Tab stops
        pane_tabs,
    }
);

format_struct!(
    /// `Format.list_windows.Struct()`
    struct ListWindowsFormat {
        session_id,
        window_id,
        window_width,
        window_height,
        window_layout,
    }
);

format_struct!(
    /// `Format.tmux_version.Struct()`
    struct TmuxVersionFormat { version }
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::point::{Coordinate, Point};
    use crate::terminal::screen::Screen;

    type CheckFn = fn(&Viewer, &[Action<'_>]);
    type CheckCommandFn = fn(&Viewer, &[u8]);

    struct TestStep {
        input: Input<'static>,
        contains_tags: &'static [ActionTag],
        contains_command: &'static str,
        check: Option<CheckFn>,
        check_command: Option<CheckCommandFn>,
    }

    impl TestStep {
        fn run(&self, viewer: &mut Viewer) {
            let actions = viewer.next(self.input);
            let viewer = actions.viewer();

            // Common mistake, forgetting the newline on a command.
            for action in actions.iter() {
                if let Action::Command(command) = action {
                    assert!(command.ends_with(b"\n"));
                }
            }

            for tag in self.contains_tags {
                assert!(actions.iter().any(|action| action.tag() == *tag), "missing action {tag:?}");
            }

            if !self.contains_command.is_empty() {
                assert!(
                    actions.iter().any(|action| matches!(
                        action,
                        Action::Command(command) if command.starts_with(self.contains_command.as_bytes())
                    )),
                    "missing command {}",
                    self.contains_command
                );
            }

            if let Some(check_fn) = self.check {
                check_fn(viewer, &actions);
            }

            if let Some(check_fn) = self.check_command {
                let mut found = false;
                for action in actions.iter() {
                    if let Action::Command(command) = action {
                        found = true;
                        check_fn(viewer, command);
                    }
                }
                assert!(found);
            }
        }
    }

    fn step(input: Input<'static>) -> TestStep {
        TestStep {
            input,
            contains_tags: &[],
            contains_command: "",
            check: None,
            check_command: None,
        }
    }

    fn block_end(content: &'static str) -> Input<'static> {
        Input::Tmux(Notification::BlockEnd(content.as_bytes()))
    }

    fn session_changed(id: usize, name: &'static str) -> Input<'static> {
        Input::Tmux(Notification::SessionChanged {
            id,
            name: name.as_bytes(),
        })
    }

    fn layout_change_input(window_id: usize, layout: &'static str) -> Input<'static> {
        Input::Tmux(Notification::LayoutChange {
            window_id,
            layout: layout.as_bytes(),
            visible_layout: layout.as_bytes(),
            raw_flags: b"*",
        })
    }

    const EXIT: Input<'static> = Input::Tmux(Notification::Exit);

    fn exit_step() -> TestStep {
        TestStep {
            contains_tags: &[ActionTag::Exit],
            ..step(EXIT)
        }
    }

    /// A helper to run a series of test steps against a viewer and assert
    /// that the expected actions are produced.
    ///
    /// I'm generally not a fan of these types of abstracted tests because
    /// it makes diagnosing failures harder, but being able to construct
    /// simulated tmux inputs and verify outputs is going to be extremely
    /// important since the tmux control mode protocol is very complex and
    /// fragile.
    fn test_viewer(viewer: &mut Viewer, steps: &[TestStep]) {
        for (i, s) in steps.iter().enumerate() {
            log::debug!(target: LOG, "testViewer step i={i}");
            s.run(viewer);
        }
    }

    fn contains(haystack: &[u8], needle: &str) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle.as_bytes())
    }

    fn count(haystack: &str, needle: &str) -> usize {
        haystack.matches(needle).count()
    }

    fn pane(v: &Viewer, id: usize) -> &Pane {
        v.panes.get(&id).unwrap()
    }

    fn active_screen(v: &Viewer, id: usize) -> &Screen {
        // SAFETY: the screen is owned by the pane's terminal.
        unsafe { &*pane(v, id).terminal.screens.active.as_ptr() }
    }

    fn dump_history(screen: &Screen) -> String {
        screen.dump_string_alloc(Point::History(Coordinate::default())).unwrap()
    }

    fn dump_active(screen: &Screen) -> String {
        screen.dump_string_alloc(Point::Active(Coordinate::default())).unwrap()
    }

    /// The startup steps shared by most tests: initial block, session,
    /// version, and a single-pane window list.
    fn startup_one_pane(first_check: Option<CheckFn>) -> Vec<TestStep> {
        vec![
            // Initial startup
            step(block_end("")),
            TestStep {
                contains_command: "display-message",
                ..step(session_changed(1, "test"))
            },
            // Receive version response, which triggers list-windows
            TestStep {
                contains_command: "list-windows",
                ..step(block_end("3.5a"))
            },
            // Receive initial window layout with one pane
            TestStep {
                contains_tags: &[ActionTag::Windows, ActionTag::Command],
                check: first_check,
                ..step(block_end("$0 @0 83 44 b7dd,83x44,0,0,0"))
            },
        ]
    }

    fn check_one_window_one_pane(v: &Viewer, _: &[Action<'_>]) {
        assert_eq!(1, v.windows.len());
        assert_eq!(1, v.panes.len());
        assert!(v.panes.contains_key(&0));
    }

    #[test]
    fn immediate_exit() {
        let mut viewer = Viewer::init();
        test_viewer(
            &mut viewer,
            &[
                exit_step(),
                TestStep {
                    check: Some(|_, actions| assert_eq!(0, actions.len())),
                    ..step(EXIT)
                },
            ],
        );
    }

    #[test]
    fn session_changed_resets_state() {
        let mut viewer = Viewer::init();
        test_viewer(
            &mut viewer,
            &[
                // Initial startup
                step(block_end("")),
                TestStep {
                    contains_command: "display-message",
                    ..step(session_changed(1, "first"))
                },
                // Receive version response, which triggers list-windows
                TestStep {
                    contains_command: "list-windows",
                    ..step(block_end("3.5a"))
                },
                // Receive window layout with two panes (same format as "initial flow" test)
                TestStep {
                    contains_tags: &[ActionTag::Windows, ActionTag::Command],
                    check: Some(|v, actions| {
                        assert_eq!(1, v.session_id);
                        assert_eq!(1, v.windows.len());
                        assert_eq!(2, v.panes.len());
                        assert_eq!(b"3.5a", &v.tmux_version[..]);

                        for action in actions {
                            if let Action::Windows(windows) = action {
                                // The action must reference viewer-owned state,
                                // not the temporary list used while parsing.
                                assert_eq!(v.windows.as_ptr(), windows.as_ptr());
                                assert_eq!(v.windows.len(), windows.len());
                                assert_eq!(0, windows[0].id);
                                return;
                            }
                        }
                        panic!("TestExpectedWindowsAction");
                    }),
                    ..step(block_end("$1 @0 83 44 027b,83x44,0,0[83x20,0,0,0,83x23,0,21,1]"))
                },
                // Now session changes - should reset everything but keep version
                TestStep {
                    contains_tags: &[ActionTag::Windows, ActionTag::Command],
                    contains_command: "list-windows",
                    check: Some(|v, actions| {
                        // Session ID should be updated
                        assert_eq!(2, v.session_id);
                        // Windows should be cleared (empty windows action sent)
                        assert!(actions
                            .iter()
                            .any(|action| matches!(action, Action::Windows(w) if w.is_empty())));
                        // Old windows should be cleared
                        assert_eq!(0, v.windows.len());
                        // Old panes should be cleared
                        assert_eq!(0, v.panes.len());
                        // Version should still be preserved
                        assert_eq!(b"3.5a", &v.tmux_version[..]);
                    }),
                    ..step(session_changed(2, "second"))
                },
                // Receive new window layout for new session (same layout, different session/window)
                // Uses same pane IDs 0,1 - they should be re-created since old panes were cleared
                TestStep {
                    contains_tags: &[ActionTag::Windows, ActionTag::Command],
                    check: Some(|v, _| {
                        assert_eq!(2, v.session_id);
                        assert_eq!(1, v.windows.len());
                        assert_eq!(1, v.windows[0].id);
                        // Panes 0 and 1 should be created (fresh, since old ones were cleared)
                        assert_eq!(2, v.panes.len());
                    }),
                    ..step(block_end("$2 @1 83 44 027b,83x44,0,0[83x20,0,0,0,83x23,0,21,1]"))
                },
                exit_step(),
            ],
        );
    }

    fn capture_step(check_command: CheckCommandFn) -> TestStep {
        TestStep {
            contains_command: "capture-pane",
            check_command: Some(check_command),
            ..step(block_end(""))
        }
    }

    #[test]
    fn initial_flow() {
        let mut viewer = Viewer::init();
        test_viewer(
            &mut viewer,
            &[
                step(block_end("")),
                TestStep {
                    contains_command: "display-message",
                    check: Some(|v, _| assert_eq!(42, v.session_id)),
                    ..step(session_changed(42, "main"))
                },
                // Receive version response, which triggers list-windows
                TestStep {
                    contains_command: "list-windows",
                    check: Some(|v, _| assert_eq!(b"3.5a", &v.tmux_version[..])),
                    ..step(block_end("3.5a"))
                },
                TestStep {
                    contains_tags: &[ActionTag::Windows, ActionTag::Command],
                    contains_command: "capture-pane",
                    // pane_history for pane 0 (primary)
                    check_command: Some(|_, command| {
                        assert!(contains(command, "-t %0"));
                        assert!(!contains(command, "-a"));
                    }),
                    ..step(block_end("$0 @0 83 44 027b,83x44,0,0[83x20,0,0,0,83x23,0,21,1]"))
                },
                TestStep {
                    // Moves on to pane_visible for pane 0 (primary)
                    contains_command: "capture-pane",
                    check_command: Some(|_, command| {
                        assert!(contains(command, "-t %0"));
                        assert!(!contains(command, "-a"));
                    }),
                    check: Some(|v, _| {
                        let screen = active_screen(v, 0);
                        assert_eq!("Hello, world!", dump_history(screen));
                        assert_eq!("", dump_active(screen));
                    }),
                    ..step(block_end("Hello, world!"))
                },
                // Moves on to pane_history for pane 0 (alternate)
                capture_step(|_, command| {
                    assert!(contains(command, "-t %0"));
                    assert!(contains(command, "-a"));
                }),
                // Moves on to pane_visible for pane 0 (alternate)
                capture_step(|_, command| {
                    assert!(contains(command, "-t %0"));
                    assert!(contains(command, "-a"));
                }),
                // Moves on to pane_history for pane 1 (primary)
                capture_step(|_, command| {
                    assert!(contains(command, "-t %1"));
                    assert!(!contains(command, "-a"));
                }),
                // Moves on to pane_visible for pane 1 (primary)
                capture_step(|_, command| {
                    assert!(contains(command, "-t %1"));
                    assert!(!contains(command, "-a"));
                }),
                // Moves on to pane_history for pane 1 (alternate)
                capture_step(|_, command| {
                    assert!(contains(command, "-t %1"));
                    assert!(contains(command, "-a"));
                }),
                // Moves on to pane_visible for pane 1 (alternate)
                capture_step(|_, command| {
                    assert!(contains(command, "-t %1"));
                    assert!(contains(command, "-a"));
                }),
                step(block_end("")),
                TestStep {
                    check: Some(|v, actions| {
                        assert_eq!(0, actions.len());
                        let str = dump_active(active_screen(v, 0));
                        assert!(str.contains("new output"));
                    }),
                    ..step(Input::Tmux(Notification::Output {
                        pane_id: 0,
                        data: b"new output",
                    }))
                },
                TestStep {
                    check: Some(|_, actions| assert_eq!(0, actions.len())),
                    ..step(Input::Tmux(Notification::Output {
                        pane_id: 999,
                        data: b"ignored",
                    }))
                },
                exit_step(),
            ],
        );
    }

    #[test]
    fn layout_change() {
        let mut viewer = Viewer::init();
        let mut steps = startup_one_pane(Some(check_one_window_one_pane));
        // Complete all capture-pane commands for pane 0 (primary and alternate)
        // plus pane_state
        for _ in 0..5 {
            steps.push(step(block_end("")));
        }
        // Now send a layout_change that splits into two panes
        steps.push(TestStep {
            contains_tags: &[ActionTag::Windows],
            check: Some(|v, _| {
                // Should still have 1 window
                assert_eq!(1, v.windows.len());
                // Should now have 2 panes (0 and 2)
                assert_eq!(2, v.panes.len());
                assert!(v.panes.contains_key(&0));
                assert!(v.panes.contains_key(&2));
                // Commands should be queued for the new pane (4 capture-pane + 1 pane_state)
                assert_eq!(5, v.command_queue.len());
            }),
            ..step(layout_change_input(0, "e07b,83x44,0,0[83x22,0,0,0,83x21,0,23,2]"))
        });
        steps.push(exit_step());
        test_viewer(&mut viewer, &steps);
    }

    fn defunct_layout_test(layout: &'static str) {
        let mut viewer = Viewer::init();
        let mut steps = startup_one_pane(Some(check_one_window_one_pane));
        // Complete all capture-pane commands for pane 0 (primary and alternate)
        // plus pane_state
        for _ in 0..5 {
            steps.push(step(block_end("")));
        }
        steps.push(TestStep {
            contains_tags: &[ActionTag::Exit],
            check: Some(|v, _| {
                assert_eq!(State::Defunct, v.state);
                assert!(!v.panes.contains_key(&2));
            }),
            ..step(layout_change_input(0, layout))
        });
        test_viewer(&mut viewer, &steps);
    }

    #[test]
    fn layout_change_with_zero_width_pane_becomes_defunct() {
        // Send a layout_change with a valid checksum but a zero-width pane.
        defunct_layout_test("52aa,83x44,0,0[83x22,0,0,0,0x21,0,23,2]");
    }

    #[test]
    fn layout_change_with_oversized_pane_becomes_defunct() {
        // Send a layout_change with a valid checksum but a pane wider than
        // our max column count.
        defunct_layout_test("3afd,83x44,0,0[83x22,0,0,0,70000x21,0,23,2]");
    }

    #[test]
    fn layout_change_does_not_return_command_when_queue_not_empty() {
        let mut viewer = Viewer::init();
        let mut steps = startup_one_pane(Some(|v, _| assert!(!v.command_queue.empty())));
        // Do NOT complete capture-pane commands - queue still has commands.
        // Send a layout_change that splits into two panes.
        // This should NOT return a command action since queue was not empty.
        steps.push(TestStep {
            contains_tags: &[ActionTag::Windows],
            check: Some(|v, actions| {
                assert_eq!(2, v.panes.len());
                // Should not contain a command action
                for action in actions {
                    assert!(action.tag() != ActionTag::Command);
                }
            }),
            ..step(layout_change_input(0, "e07b,83x44,0,0[83x22,0,0,0,83x21,0,23,2]"))
        });
        steps.push(exit_step());
        test_viewer(&mut viewer, &steps);
    }

    #[test]
    fn layout_change_returns_command_when_queue_was_empty() {
        let mut viewer = Viewer::init();
        let mut steps = startup_one_pane(None);
        // Complete all capture-pane commands for pane 0
        for _ in 0..4 {
            steps.push(step(block_end("")));
        }
        // Queue should now be empty
        steps.push(TestStep {
            check: Some(|v, _| assert!(v.command_queue.empty())),
            ..step(block_end(""))
        });
        // Now send a layout_change that splits into two panes.
        // This should return a command action since we're queuing commands
        // for the new pane and the queue was empty.
        steps.push(TestStep {
            contains_tags: &[ActionTag::Windows, ActionTag::Command],
            check: Some(|v, _| {
                assert_eq!(2, v.panes.len());
                assert!(!v.command_queue.empty());
            }),
            ..step(layout_change_input(0, "e07b,83x44,0,0[83x22,0,0,0,83x21,0,23,2]"))
        });
        steps.push(exit_step());
        test_viewer(&mut viewer, &steps);
    }

    #[test]
    fn window_add_queues_list_windows_when_queue_empty() {
        let mut viewer = Viewer::init();
        let mut steps = startup_one_pane(None);
        // Complete all capture-pane commands for pane 0
        for _ in 0..4 {
            steps.push(step(block_end("")));
        }
        // Queue should now be empty
        steps.push(TestStep {
            check: Some(|v, _| assert!(v.command_queue.empty())),
            ..step(block_end(""))
        });
        // Now send window_add - should trigger list-windows command
        steps.push(TestStep {
            contains_command: "list-windows",
            check: Some(|v, _| {
                // Command queue should have list_windows
                assert!(!v.command_queue.empty());
                assert_eq!(1, v.command_queue.len());
            }),
            ..step(Input::Tmux(Notification::WindowAdd { id: 1 }))
        });
        steps.push(exit_step());
        test_viewer(&mut viewer, &steps);
    }

    #[test]
    fn window_add_queues_list_windows_when_queue_not_empty() {
        let mut viewer = Viewer::init();
        // Queue should have capture-pane commands
        let mut steps = startup_one_pane(Some(|v, _| assert!(!v.command_queue.empty())));
        // Do NOT complete capture-pane commands - queue still has commands.
        // Send window_add - should queue list-windows but NOT return command action
        steps.push(TestStep {
            check: Some(|v, actions| {
                // Should not contain a command action since queue was not empty
                for action in actions {
                    assert!(action.tag() != ActionTag::Command);
                }
                // But list_windows should be in the queue
                assert!(!v.command_queue.empty());
            }),
            ..step(Input::Tmux(Notification::WindowAdd { id: 1 }))
        });
        steps.push(exit_step());
        test_viewer(&mut viewer, &steps);
    }

    #[test]
    fn two_pane_flow_with_pane_state() {
        let mut viewer = Viewer::init();
        test_viewer(
            &mut viewer,
            &[
                // Initial block_end from attach
                step(block_end("")),
                // Session changed notification
                TestStep {
                    contains_command: "display-message",
                    check: Some(|v, _| assert_eq!(0, v.session_id)),
                    ..step(session_changed(0, "0"))
                },
                // Receive version response, which triggers list-windows
                TestStep {
                    contains_command: "list-windows",
                    ..step(block_end("3.5a"))
                },
                // list-windows output with 2 panes in a vertical split
                TestStep {
                    contains_tags: &[ActionTag::Windows, ActionTag::Command],
                    check: Some(|v, _| {
                        assert_eq!(1, v.windows.len());
                        let window = &v.windows[0];
                        assert_eq!(0, window.id);
                        assert_eq!(165, window.width);
                        assert_eq!(79, window.height);
                        assert_eq!(2, v.panes.len());
                        assert!(v.panes.contains_key(&0));
                        assert!(v.panes.contains_key(&4));
                    }),
                    ..step(block_end("$0 @0 165 79 ca97,165x79,0,0[165x40,0,0,0,165x38,0,41,4]"))
                },
                // capture-pane pane 0 primary history
                step(block_end("prompt %\nprompt %")),
                // capture-pane pane 0 primary visible
                TestStep {
                    check: Some(|v, _| {
                        let screen = active_screen(v, 0);
                        // History has 2 lines with "prompt %" (padded to screen width)
                        assert!(count(&dump_history(screen), "prompt %") >= 2);
                        assert_eq!("prompt %", dump_active(screen));
                    }),
                    ..step(block_end("prompt %"))
                },
                // capture-pane pane 0 alternate history (empty)
                step(block_end("")),
                // capture-pane pane 0 alternate visible (empty)
                step(block_end("")),
                // capture-pane pane 4 primary history
                step(block_end("prompt %")),
                // capture-pane pane 4 primary visible
                TestStep {
                    check: Some(|v, _| {
                        let screen = active_screen(v, 4);
                        assert_eq!("prompt %", dump_history(screen));
                        // Active screen starts with "prompt %" at beginning
                        assert!(dump_active(screen).starts_with("prompt %"));
                    }),
                    ..step(block_end("prompt %"))
                },
                // capture-pane pane 4 alternate history (empty)
                step(block_end("")),
                // capture-pane pane 4 alternate visible (empty)
                step(block_end("")),
                // list-panes output with terminal state
                TestStep {
                    check: Some(|v, _| {
                        // Pane 0: cursor at (42, 0), cursor visible, wraparound on
                        {
                            let t = &pane(v, 0).terminal;
                            let screen = t.screens.get(ScreenKey::Primary).unwrap();
                            assert_eq!(42, screen.cursor.x);
                            assert_eq!(0, screen.cursor.y);
                            assert!(t.modes.get(Mode::CursorVisible));
                            assert!(t.modes.get(Mode::Wraparound));
                            assert!(!t.modes.get(Mode::Insert));
                            assert!(!t.modes.get(Mode::Origin));
                            assert!(!t.modes.get(Mode::KeypadKeys));
                            assert!(!t.modes.get(Mode::CursorKeys));
                            assert!(t.modes.get(Mode::MouseEventButton));
                            assert!(!t.modes.get(Mode::MouseEventNormal));
                            assert!(t.modes.get(Mode::MouseFormatSgr));
                        }
                        // Pane 4: cursor at (10, 5), cursor visible, wraparound on
                        {
                            let t = &pane(v, 4).terminal;
                            let screen = t.screens.get(ScreenKey::Primary).unwrap();
                            assert_eq!(10, screen.cursor.x);
                            assert_eq!(5, screen.cursor.y);
                            assert!(t.modes.get(Mode::CursorVisible));
                            assert!(t.modes.get(Mode::Wraparound));
                            assert!(!t.modes.get(Mode::Insert));
                            assert!(!t.modes.get(Mode::Origin));
                            assert!(!t.modes.get(Mode::KeypadKeys));
                            assert!(!t.modes.get(Mode::CursorKeys));
                        }
                    }),
                    ..step(block_end(
                        "%0;42;0;1;;;;0;4294967295;4294967295;0;1;0;0;0;0;1;0;0;1;;;0;39;8,16,24,32,40,48,56,64,72,80,88,96,104,112,120,128,136,144,152,160\n%4;10;5;1;;;;0;4294967295;4294967295;0;1;0;0;0;0;0;0;0;0;;;0;37;8,16,24,32,40,48,56,64,72,80,88,96,104,112,120,128,136,144,152,160",
                    ))
                },
                exit_step(),
            ],
        );
    }

    fn viewer_with_pane_42() -> Viewer {
        let mut viewer = Viewer::init();
        let terminal = Terminal::init(TerminalOptions::new(80, 24)).unwrap();
        viewer.panes.insert(42, Pane::new(terminal));
        viewer
    }

    #[test]
    fn tmux_output_state_unsplit_csi() {
        let mut viewer = viewer_with_pane_42();
        viewer.received_output(42, b"\x1b[32mX").unwrap();
        assert_eq!("X", dump_active(active_screen(&viewer, 42)));
    }

    #[test]
    fn tmux_output_state_split_csi() {
        let mut viewer = viewer_with_pane_42();
        viewer.received_output(42, b"\x1b[32").unwrap();
        viewer.received_output(42, b"mX").unwrap();
        assert_eq!("X", dump_active(active_screen(&viewer, 42)));
    }

    #[test]
    fn pane_state_ignores_out_of_bounds_scroll_region() {
        let mut viewer = Viewer::init();
        test_viewer(
            &mut viewer,
            &[
                step(block_end("")),
                TestStep {
                    contains_command: "display-message",
                    ..step(session_changed(0, "0"))
                },
                TestStep {
                    contains_command: "list-windows",
                    ..step(block_end("3.5a"))
                },
                TestStep {
                    contains_tags: &[ActionTag::Windows, ActionTag::Command],
                    ..step(block_end("$0 @0 80 24 b25d,80x24,0,0,0"))
                },
                // capture-pane primary/alternate history/visible
                step(block_end("")),
                step(block_end("")),
                step(block_end("")),
                step(block_end("")),
                // list-panes with a scroll region bottom far past the screen
                TestStep {
                    check: Some(|v, _| {
                        let t = &pane(v, 0).terminal;
                        // Verify the line parsed and was applied.
                        assert_eq!(5, t.screens.active.cursor.y);
                        assert_eq!(0, t.scrolling_region.top);
                        assert_eq!(t.rows - 1, t.scrolling_region.bottom);
                    }),
                    ..step(block_end(
                        "%0;0;5;1;;;0;0;4294967295;4294967295;0;1;0;0;0;0;0;0;0;0;0;0;0;60000;",
                    ))
                },
                // Vertical tabs act as linefeeds and must scroll within the screen.
                TestStep {
                    check: Some(|v, _| {
                        let t = &pane(v, 0).terminal;
                        assert_eq!(t.rows - 1, t.screens.active.cursor.y);
                    }),
                    ..step(Input::Tmux(Notification::Output {
                        pane_id: 0,
                        data: &[0x0b; 30],
                    }))
                },
            ],
        );
    }

    #[test]
    fn tmux_raw_startup_selects_primary_screen() {
        test_raw_startup_screen(false);
    }

    #[test]
    fn tmux_raw_startup_selects_alternate_screen() {
        test_raw_startup_screen(true);
    }

    fn test_raw_startup_screen(alternate_on: bool) {
        let mut parser = control::Parser::default();
        let mut viewer = Viewer::init();

        let input = [
            // Initial connection.
            "%begin 1 1 0\n",
            "%end 1 1 0\n",
            "%session-changed $42 main\n",
            // Tmux version response.
            "%begin 2 2 0\n",
            "3.5a\n",
            "%end 2 2 0\n",
            // Window and pane layout.
            "%begin 3 3 0\n",
            "$42 @0 80 24 d962,80x24,0,0,42\n",
            "%end 3 3 0\n",
            // Primary screen history.
            "%begin 4 4 0\n",
            "%end 4 4 0\n",
            // Primary screen visible contents.
            "%begin 5 5 0\n",
            "%end 5 5 0\n",
            // Alternate screen history.
            "%begin 6 6 0\n",
            "%end 6 6 0\n",
            // Alternate screen visible contents.
            "%begin 7 7 0\n",
            "%end 7 7 0\n",
            // Cursor position and terminal modes.
            "%begin 8 8 0\n",
            "%42;0;0;1;block;default;0;",
            if alternate_on { "1;0;0;" } else { "0;0;0;" },
            "0;1;0;0;0;",
            "0;0;0;0;0;",
            "0;0;0;23;8,16,24,32,40,48,56,64,72\n",
            "%end 8 8 0\n",
            // Live output after initialization.
            "%output %42 hello\n",
        ]
        .concat();

        let expected_commands: [&str; 7] = [
            "display-message",
            "list-windows",
            "capture-pane -p -e -q -S - -E -1 -t %42\n",
            "capture-pane -p -e -q -t %42\n",
            "capture-pane -p -e -q -a -S - -E -1 -t %42\n",
            "capture-pane -p -e -q -a -t %42\n",
            "list-panes",
        ];

        let mut command_index: usize = 0;

        for &byte in input.as_bytes() {
            let Some(notification) = parser.put(byte).unwrap() else {
                continue;
            };
            let actions = viewer.next(Input::Tmux(notification));
            for action in actions.iter() {
                match action {
                    Action::Command(command) => {
                        assert!(command_index < expected_commands.len());
                        let expected = expected_commands[command_index];
                        assert!(command.starts_with(expected.as_bytes()));
                        command_index += 1;
                    }
                    Action::Windows(_) => {}
                    Action::Exit => panic!("UnexpectedViewerExit"),
                }
            }
        }
        assert_eq!(expected_commands.len(), command_index);
        assert_eq!(42, viewer.session_id);
        assert_eq!(b"3.5a", &viewer.tmux_version[..]);

        assert_eq!(1, viewer.windows.len());
        assert_eq!(1, viewer.panes.len());
        assert!(viewer.panes.contains_key(&42));
        assert!(viewer.command_queue.empty());
        let pane = viewer.panes.get(&42).unwrap();

        let expected_key = if alternate_on {
            ScreenKey::Alternate
        } else {
            ScreenKey::Primary
        };
        assert_eq!(expected_key, pane.terminal.screens.active_key);

        assert_eq!("hello", dump_active(active_screen(&viewer, 42)));

        let other_key = if alternate_on {
            ScreenKey::Primary
        } else {
            ScreenKey::Alternate
        };
        let other_screen = pane.terminal.screens.get(other_key).unwrap();
        assert_eq!("", dump_active(&other_screen));
        parser.deinit();
    }
}
