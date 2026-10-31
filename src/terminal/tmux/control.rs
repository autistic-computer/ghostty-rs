//! Port of `src/terminal/tmux/control.zig`.
//!
//! STATUS: ported
//!
//! This file contains the implementation for tmux control mode. See
//! tmux(1) for more information on control mode. Some basics are documented
//! here but this is not meant to be a comprehensive source of protocol
//! documentation.
//!
//! Port notes:
//! - `std.Io.Writer.Allocating` is a `Vec<u8>`; "deinit" in the broken
//!   state frees it by replacing it with an empty `Vec`.
//! - Notifications borrow from the parser buffer exactly like the Zig
//!   slices do, so a returned [`Notification`] is tied to the
//!   `&mut Parser` borrow of [`Parser::put`].
//! - Oniguruma must be initialized (`crate::pkg::oniguruma::init`) before
//!   regex-based notifications can be parsed, as in Zig.

use std::fmt;

use crate::pkg::oniguruma as oni;
use crate::terminal::tmux::output::parse_int_usize;

const LOG_TARGET: &str = "terminal_tmux";

/// Allocation failure (Zig: `Allocator.Error`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocError {
    OutOfMemory,
}

impl fmt::Display for AllocError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for AllocError {}

/// A tmux control mode parser. This takes in output from tmux control
/// mode and parses it into a structured notifications.
///
/// It is up to the caller to establish the connection to the tmux
/// control mode session in some way (e.g. via exec, a network socket,
/// whatever). This is fully agnostic to how the data is received and sent.
#[derive(Debug)]
pub struct Parser {
    /// Current state of the client.
    pub state: State,

    /// The buffer used to store in-progress notifications, output, etc.
    pub buffer: Vec<u8>,

    /// The maximum size in bytes of the buffer. This is used to limit
    /// memory usage. If the buffer exceeds this size, the client will
    /// enter a broken state (the control mode session will be forcibly
    /// exited and future data dropped).
    pub max_bytes: usize,
}

impl Default for Parser {
    fn default() -> Self {
        Parser {
            state: State::Idle,
            buffer: Vec::new(),
            max_bytes: 1024 * 1024,
        }
    }
}

/// `Parser.State`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Outside of any active notifications. This should drop any output
    /// unless it is '%' on the first byte of a line. The buffer will be
    /// cleared when it sees '%', this is so that the previous notification
    /// data is valid until we receive/process new data.
    Idle,

    /// We experienced unexpected input and are in a broken state
    /// so we cannot continue processing. When this state is set,
    /// the buffer has been deinited and must not be accessed.
    Broken,

    /// Inside an active notification (started with '%').
    Notification,

    /// Inside a begin/end block.
    Block,
}

/// `Parser.ParseError`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParseError {
    RegexError,
}

/// `Parser.BlockTerminator`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockTerminator {
    End,
    Err,
}

/// The capture group byte ranges of a successful regex search.
type Captures = Vec<(usize, usize)>;

impl Parser {
    pub fn deinit(&mut self) {
        // If we're in a broken state, we already deinited
        // the buffer, so we don't need to do anything.
        if self.state == State::Broken {
            return;
        }

        self.buffer = Vec::new();
    }

    // Handle a byte of input.
    //
    // If we reach our byte limit this will return OutOfMemory. It only
    // does this on the first time we exceed the limit; subsequent calls
    // will return null as we drop all input in a broken state.
    pub fn put(&mut self, byte: u8) -> Result<Option<Notification<'_>>, AllocError> {
        // If we're in a broken state, just do nothing.
        //
        // We have to do this check here before we check the buffer, because if
        // we're in a broken state then we'd have already deinited the buffer.
        if self.state == State::Broken {
            return Ok(None);
        }

        match self.state {
            // Drop because we're in a broken state.
            State::Broken => return Ok(None),

            // Waiting for a notification so if the byte is not '%' then
            // we're in a broken state. Control mode output should always
            // be wrapped in '%begin/%end' orelse we expect a notification.
            // Return an exit notification.
            State::Idle => {
                if byte != b'%' {
                    self.broken();
                    return Ok(Some(Notification::Exit));
                } else {
                    self.buffer.clear();
                    self.state = State::Notification;
                }
            }

            // If we're in a notification and its not a newline then
            // we accumulate. If it is a newline then we have a
            // complete notification we need to parse.
            State::Notification => {
                if byte == b'\n' {
                    // We have a complete notification, parse it.
                    //
                    // If parsing failed, then we do not mark the state
                    // as broken because we may be able to continue parsing
                    // other types of notifications.
                    //
                    // In the future we may want to emit a notification
                    // here about unknown or unsupported notifications.
                    return Ok(self.parse_notification().unwrap_or(None));
                }
            }

            // If we're in a block then we accumulate until we see a newline
            // and then we check to see if that line ended the block.
            State::Block => {
                if byte == b'\n' {
                    let written = &self.buffer[..];
                    let idx = match memchr::memrchr(b'\n', written) {
                        Some(v) => v + 1,
                        None => 0,
                    };
                    let line = &written[idx..];

                    if let Some(terminator) = parse_block_terminator(line) {
                        let output_len = trim_end_len(&written[..idx], b"\r\n");

                        // Important: do not clear buffer since the notification
                        // contains it.
                        self.state = State::Idle;
                        let output = &self.buffer[..output_len];
                        return Ok(Some(match terminator {
                            BlockTerminator::End => Notification::BlockEnd(output),
                            BlockTerminator::Err => {
                                log::warn!(
                                    target: LOG_TARGET,
                                    "tmux control mode error={}",
                                    String::from_utf8_lossy(output)
                                );
                                Notification::BlockErr(output)
                            }
                        }));
                    }

                    // Didn't end the block, continue accumulating.
                }
            }
        }

        // Only enforce the limit on bytes we actually store, so a line
        // filling the buffer exactly can still be terminated.
        if self.buffer.len() >= self.max_bytes {
            self.broken();
            return Err(AllocError::OutOfMemory);
        }

        if self.buffer.try_reserve(1).is_err() {
            return Err(AllocError::OutOfMemory);
        }
        self.buffer.push(byte);

        Ok(None)
    }

    /// Compile `pattern` and search `line` with it, returning the capture
    /// group ranges. `Err` is a regex initialization failure; `Ok(None)`
    /// means the line did not match (the Zig code logs and breaks out of
    /// the command block in that case).
    fn search(pattern: &[u8], cmd: &[u8], line: &[u8]) -> Result<Option<Captures>, ParseError> {
        let mut re = match oni::Regex::init(
            pattern,
            oni::types::Option {
                capture_group: true,
                ..Default::default()
            },
            oni::Encoding::utf8(),
            oni::Syntax::default(),
            None,
        ) {
            Ok(re) => re,
            Err(err) => {
                log::warn!(target: LOG_TARGET, "regex init failed error={:?}", err);
                return Err(ParseError::RegexError);
            }
        };

        let region = match re.search(line, oni::types::Option::default()) {
            Ok(region) => region,
            Err(err) => {
                log::warn!(
                    target: LOG_TARGET,
                    "failed to match notification cmd={} line=\"{}\" err={:?}",
                    String::from_utf8_lossy(cmd),
                    String::from_utf8_lossy(line),
                    err
                );
                return Ok(None);
            }
        };

        let starts = region.starts();
        let ends = region.ends();
        Ok(Some(
            starts
                .iter()
                .zip(ends.iter())
                .map(|(&s, &e)| (s as usize, e as usize))
                .collect(),
        ))
    }

    fn parse_notification(&mut self) -> Result<Option<Notification<'_>>, ParseError> {
        debug_assert!(self.state == State::Notification);

        let line_len = {
            let line = &self.buffer[..];
            if line[line.len() - 1] == b'\r' {
                line.len() - 1
            } else {
                line.len()
            }
        };
        let cmd_len = {
            let line = &self.buffer[..line_len];
            memchr::memchr(b' ', line).unwrap_or(line.len())
        };

        // Helpers to parse a captured integer. The regex guarantees the
        // capture is all digits; Zig uses `catch unreachable`.
        fn int(line: &[u8], cap: (usize, usize)) -> usize {
            parse_int_usize(&line[cap.0..cap.1]).unwrap()
        }

        // The notification MUST exist because we guard entering the notification
        // state on seeing at least a '%'.
        'cmd: {
            let line = &self.buffer[..line_len];
            let cmd = &line[..cmd_len];
            if cmd == b"%begin" {
                // We don't use the rest of the tokens for now because tmux
                // claims to guarantee that begin/end are always in order and
                // never intermixed. In the future, we should probably validate
                // this.
                // TODO(tmuxcc): do this before merge?

                // Move to block state because we expect a corresponding end/error
                // and want to accumulate the data.
                self.state = State::Block;
                self.buffer.clear();
                return Ok(None);
            } else if cmd == b"%output" {
                let Some(caps) = Self::search(b"^%output %([0-9]+) (.+)$", cmd, line)? else {
                    break 'cmd;
                };
                let id = int(line, caps[1]);
                let data = caps[2];

                // Important: do not clear buffer here since name points to it
                self.state = State::Idle;
                let line = &self.buffer[..line_len];
                return Ok(Some(Notification::Output {
                    pane_id: id,
                    data: &line[data.0..data.1],
                }));
            } else if cmd == b"%session-changed" {
                let Some(caps) =
                    Self::search(b"^%session-changed \\$([0-9]+) (.+)$", cmd, line)?
                else {
                    break 'cmd;
                };
                let id = int(line, caps[1]);
                let name = caps[2];

                // Important: do not clear buffer here since name points to it
                self.state = State::Idle;
                let line = &self.buffer[..line_len];
                return Ok(Some(Notification::SessionChanged {
                    id,
                    name: &line[name.0..name.1],
                }));
            } else if cmd == b"%sessions-changed" {
                if line != b"%sessions-changed" {
                    log::warn!(
                        target: LOG_TARGET,
                        "failed to match notification cmd={} line=\"{}\"",
                        String::from_utf8_lossy(cmd),
                        String::from_utf8_lossy(line)
                    );
                    break 'cmd;
                }

                self.buffer.clear();
                self.state = State::Idle;
                return Ok(Some(Notification::SessionsChanged));
            } else if cmd == b"%layout-change" {
                let Some(caps) =
                    Self::search(b"^%layout-change @([0-9]+) (.+) (.+) (.*)$", cmd, line)?
                else {
                    break 'cmd;
                };
                let id = int(line, caps[1]);
                let (layout, visible_layout, raw_flags) = (caps[2], caps[3], caps[4]);

                // Important: do not clear buffer here since layout strings point to it
                self.state = State::Idle;
                let line = &self.buffer[..line_len];
                return Ok(Some(Notification::LayoutChange {
                    window_id: id,
                    layout: &line[layout.0..layout.1],
                    visible_layout: &line[visible_layout.0..visible_layout.1],
                    raw_flags: &line[raw_flags.0..raw_flags.1],
                }));
            } else if cmd == b"%window-add" {
                let Some(caps) = Self::search(b"^%window-add @([0-9]+)$", cmd, line)? else {
                    break 'cmd;
                };
                let id = int(line, caps[1]);

                self.buffer.clear();
                self.state = State::Idle;
                return Ok(Some(Notification::WindowAdd { id }));
            } else if cmd == b"%window-renamed" {
                let Some(caps) = Self::search(b"^%window-renamed @([0-9]+) (.+)$", cmd, line)?
                else {
                    break 'cmd;
                };
                let id = int(line, caps[1]);
                let name = caps[2];

                // Important: do not clear buffer here since name points to it
                self.state = State::Idle;
                let line = &self.buffer[..line_len];
                return Ok(Some(Notification::WindowRenamed {
                    id,
                    name: &line[name.0..name.1],
                }));
            } else if cmd == b"%window-pane-changed" {
                let Some(caps) =
                    Self::search(b"^%window-pane-changed @([0-9]+) %([0-9]+)$", cmd, line)?
                else {
                    break 'cmd;
                };
                let window_id = int(line, caps[1]);
                let pane_id = int(line, caps[2]);

                self.buffer.clear();
                self.state = State::Idle;
                return Ok(Some(Notification::WindowPaneChanged { window_id, pane_id }));
            } else if cmd == b"%client-detached" {
                let Some(caps) = Self::search(b"^%client-detached (.+)$", cmd, line)? else {
                    break 'cmd;
                };
                let client = caps[1];

                // Important: do not clear buffer here since client points to it
                self.state = State::Idle;
                let line = &self.buffer[..line_len];
                return Ok(Some(Notification::ClientDetached {
                    client: &line[client.0..client.1],
                }));
            } else if cmd == b"%client-session-changed" {
                let Some(caps) = Self::search(
                    b"^%client-session-changed (.+) \\$([0-9]+) (.+)$",
                    cmd,
                    line,
                )?
                else {
                    break 'cmd;
                };
                let client = caps[1];
                let session_id = int(line, caps[2]);
                let name = caps[3];

                // Important: do not clear buffer here since client/name point to it
                self.state = State::Idle;
                let line = &self.buffer[..line_len];
                return Ok(Some(Notification::ClientSessionChanged {
                    client: &line[client.0..client.1],
                    session_id,
                    name: &line[name.0..name.1],
                }));
            } else {
                // Unknown notification, log it and return to idle state.
                log::warn!(
                    target: LOG_TARGET,
                    "unknown tmux control mode notification={}",
                    String::from_utf8_lossy(cmd)
                );
            }
        }

        // Unknown command. Clear the buffer and return to idle state.
        self.buffer.clear();
        self.state = State::Idle;

        Ok(None)
    }

    // Mark the tmux state as broken.
    fn broken(&mut self) {
        self.state = State::Broken;
        self.buffer = Vec::new();
    }
}

/// Block payload is raw data, so a line only terminates a block if it
/// exactly matches tmux's `%end`/`%error` guard-line shape.
fn parse_block_terminator(line_raw: &[u8]) -> Option<BlockTerminator> {
    let mut line = line_raw;
    if !line.is_empty() && line[line.len() - 1] == b'\r' {
        line = &line[..line.len() - 1];
    }

    // std.mem.tokenizeScalar: skips empty tokens.
    let mut fields = line.split(|&c| c == b' ').filter(|s| !s.is_empty());
    let cmd = fields.next()?;
    let terminator = if cmd == b"%end" {
        BlockTerminator::End
    } else if cmd == b"%error" {
        BlockTerminator::Err
    } else {
        return None;
    };

    let time = fields.next()?;
    let command_id = fields.next()?;
    let flags = fields.next()?;
    let extra = fields.next();

    // In the future, we should compare these to the %begin block
    // because the tmux source guarantees that these always match and
    // that is a more robust way to match.
    parse_int_usize(time).ok()?;
    parse_int_usize(command_id).ok()?;
    parse_int_usize(flags).ok()?;
    if extra.is_some() {
        return None;
    }

    Some(terminator)
}

/// Length of `s` after `std.mem.trimEnd(u8, s, values)`.
fn trim_end_len(s: &[u8], values: &[u8]) -> usize {
    let mut end = s.len();
    while end > 0 && values.contains(&s[end - 1]) {
        end -= 1;
    }
    end
}

/// Possible notification types from tmux control mode. These are documented
/// in tmux(1). A lot of the simple documentation was copied from that man
/// page here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notification<'a> {
    /// Entering tmux control mode. This isn't an actual event sent by
    /// tmux but is one sent by us to indicate that we have detected that
    /// tmux control mode is starting.
    Enter,

    /// Exit.
    ///
    /// NOTE: The tmux protocol contains a "reason" string (human friendly)
    /// associated with this. We currently drop it because we don't need it
    /// but this may be something we want to add later. If we do add it,
    /// we have to consider buffer limits and how we handle those (dropping
    /// vs truncating, etc.).
    Exit,

    /// Dispatched at the end of a begin/end block with the raw data.
    /// The control mode parser can't parse the data because it is unaware
    /// of the command that was sent to trigger this output.
    BlockEnd(&'a [u8]),
    BlockErr(&'a [u8]),

    /// Raw output from a pane.
    Output {
        pane_id: usize,
        data: &'a [u8], // unescaped
    },

    /// The client is now attached to the session with ID session-id, which is
    /// named name.
    SessionChanged { id: usize, name: &'a [u8] },

    /// A session was created or destroyed.
    SessionsChanged,

    /// The layout of the window with ID window-id changed.
    LayoutChange {
        window_id: usize,
        layout: &'a [u8],
        visible_layout: &'a [u8],
        raw_flags: &'a [u8],
    },

    /// The window with ID window-id was linked to the current session.
    WindowAdd { id: usize },

    /// The window with ID window-id was renamed to name.
    WindowRenamed { id: usize, name: &'a [u8] },

    /// The active pane in the window with ID window-id changed to the pane
    /// with ID pane-id.
    WindowPaneChanged { window_id: usize, pane_id: usize },

    /// The client has detached.
    ClientDetached { client: &'a [u8] },

    /// The client is now attached to the session with ID session-id, which is
    /// named name.
    ClientSessionChanged {
        client: &'a [u8],
        session_id: usize,
        name: &'a [u8],
    },
}

impl Notification<'_> {
    /// The Zig tag name of this notification.
    pub fn tag_name(&self) -> &'static str {
        match self {
            Notification::Enter => "enter",
            Notification::Exit => "exit",
            Notification::BlockEnd(_) => "block_end",
            Notification::BlockErr(_) => "block_err",
            Notification::Output { .. } => "output",
            Notification::SessionChanged { .. } => "session_changed",
            Notification::SessionsChanged => "sessions_changed",
            Notification::LayoutChange { .. } => "layout_change",
            Notification::WindowAdd { .. } => "window_add",
            Notification::WindowRenamed { .. } => "window_renamed",
            Notification::WindowPaneChanged { .. } => "window_pane_changed",
            Notification::ClientDetached { .. } => "client_detached",
            Notification::ClientSessionChanged { .. } => "client_session_changed",
        }
    }
}

/// `Notification.format`: `terminal.tmux.control.Notification{ .tag = value }`.
/// Raw byte payloads are printed as trimmed strings; struct payloads use
/// a Zig-like `.{ .field = value }` rendering.
impl fmt::Display for Notification<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fn s(v: &[u8]) -> String {
            let start = v
                .iter()
                .position(|c| !b" \t\r\n".contains(c))
                .unwrap_or(v.len());
            let end = trim_end_len(v, b" \t\r\n").max(start);
            String::from_utf8_lossy(&v[start..end]).into_owned()
        }

        write!(f, "terminal.tmux.control.Notification{{ .{} = ", self.tag_name())?;
        match *self {
            Notification::Enter | Notification::Exit | Notification::SessionsChanged => {
                f.write_str("void")?
            }
            Notification::BlockEnd(v) | Notification::BlockErr(v) => write!(f, "\"{}\"", s(v))?,
            Notification::Output { pane_id, data } => write!(
                f,
                ".{{ .pane_id = {}, .data = {:?} }}",
                pane_id,
                String::from_utf8_lossy(data)
            )?,
            Notification::SessionChanged { id, name } => write!(
                f,
                ".{{ .id = {}, .name = {:?} }}",
                id,
                String::from_utf8_lossy(name)
            )?,
            Notification::LayoutChange {
                window_id,
                layout,
                visible_layout,
                raw_flags,
            } => write!(
                f,
                ".{{ .window_id = {}, .layout = {:?}, .visible_layout = {:?}, .raw_flags = {:?} }}",
                window_id,
                String::from_utf8_lossy(layout),
                String::from_utf8_lossy(visible_layout),
                String::from_utf8_lossy(raw_flags)
            )?,
            Notification::WindowAdd { id } => write!(f, ".{{ .id = {} }}", id)?,
            Notification::WindowRenamed { id, name } => write!(
                f,
                ".{{ .id = {}, .name = {:?} }}",
                id,
                String::from_utf8_lossy(name)
            )?,
            Notification::WindowPaneChanged { window_id, pane_id } => write!(
                f,
                ".{{ .window_id = {}, .pane_id = {} }}",
                window_id, pane_id
            )?,
            Notification::ClientDetached { client } => write!(
                f,
                ".{{ .client = {:?} }}",
                String::from_utf8_lossy(client)
            )?,
            Notification::ClientSessionChanged {
                client,
                session_id,
                name,
            } => write!(
                f,
                ".{{ .client = {:?}, .session_id = {}, .name = {:?} }}",
                String::from_utf8_lossy(client),
                session_id,
                String::from_utf8_lossy(name)
            )?,
        }
        f.write_str(" }")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pkg::oniguruma::testing::ensure_init;

    fn new_parser() -> Parser {
        ensure_init().unwrap();
        Parser::default()
    }

    fn put_all_none(c: &mut Parser, bytes: &[u8]) {
        for &byte in bytes {
            assert!(c.put(byte).unwrap().is_none());
        }
    }

    #[test]
    fn tmux_begin_end_empty() {
        let mut c = new_parser();
        put_all_none(&mut c, b"%begin 1578922740 269 1\n");
        put_all_none(&mut c, b"%end 1578922740 269 1");
        let n = c.put(b'\n').unwrap().unwrap();
        assert_eq!(Notification::BlockEnd(b""), n);
    }

    #[test]
    fn tmux_begin_error_empty() {
        let mut c = new_parser();
        put_all_none(&mut c, b"%begin 1578922740 269 1\n");
        put_all_none(&mut c, b"%error 1578922740 269 1");
        let n = c.put(b'\n').unwrap().unwrap();
        assert_eq!(Notification::BlockErr(b""), n);
    }

    #[test]
    fn tmux_notification_exactly_max_bytes() {
        let line = b"%sessions-changed";
        let mut c = Parser {
            max_bytes: line.len(),
            ..new_parser()
        };

        // Twice, because the second '%' arrives while the buffer is still
        // full from the first notification.
        for _ in 0..2 {
            put_all_none(&mut c, line);
            let n = c.put(b'\n').unwrap().unwrap();
            assert_eq!(Notification::SessionsChanged, n);
        }
    }

    #[test]
    fn tmux_block_terminator_exactly_max_bytes() {
        let block = b"hello\n%end 1578922740 269 1";
        let mut c = Parser {
            max_bytes: block.len(),
            ..new_parser()
        };
        put_all_none(&mut c, b"%begin 1578922740 269 1\n");
        put_all_none(&mut c, block);
        let n = c.put(b'\n').unwrap().unwrap();
        assert_eq!(Notification::BlockEnd(b"hello"), n);
    }

    #[test]
    fn tmux_exceeding_max_bytes() {
        let line = b"%sessions-changed";
        let mut c = Parser {
            max_bytes: line.len() - 1,
            ..new_parser()
        };
        put_all_none(&mut c, &line[..line.len() - 1]);
        assert_eq!(Err(AllocError::OutOfMemory), c.put(line[line.len() - 1]));
        assert!(c.put(b'\n').unwrap().is_none());
    }

    #[test]
    fn tmux_begin_end_data() {
        let mut c = new_parser();
        put_all_none(&mut c, b"%begin 1578922740 269 1\n");
        put_all_none(&mut c, b"hello\nworld\n");
        put_all_none(&mut c, b"%end 1578922740 269 1");
        let n = c.put(b'\n').unwrap().unwrap();
        assert_eq!(Notification::BlockEnd(b"hello\nworld"), n);
    }

    #[test]
    fn tmux_block_payload_may_start_with_end() {
        let mut c = new_parser();
        put_all_none(&mut c, b"%begin 1 1 1\n");
        put_all_none(&mut c, b"%end not really\n");
        put_all_none(&mut c, b"hello\n");
        put_all_none(&mut c, b"%end 1 1 1");
        let n = c.put(b'\n').unwrap().unwrap();
        assert_eq!(Notification::BlockEnd(b"%end not really\nhello"), n);
    }

    #[test]
    fn tmux_block_payload_may_start_with_error() {
        let mut c = new_parser();
        put_all_none(&mut c, b"%begin 1 1 1\n");
        put_all_none(&mut c, b"%error not really\n");
        put_all_none(&mut c, b"hello\n");
        put_all_none(&mut c, b"%end 1 1 1");
        let n = c.put(b'\n').unwrap().unwrap();
        assert_eq!(Notification::BlockEnd(b"%error not really\nhello"), n);
    }

    #[test]
    fn tmux_block_may_terminate_with_real_error_after_misleading_payload() {
        let mut c = new_parser();
        put_all_none(&mut c, b"%begin 1 1 1\n");
        put_all_none(&mut c, b"%error not really\n");
        put_all_none(&mut c, b"hello\n");
        put_all_none(&mut c, b"%error 1 1 1");
        let n = c.put(b'\n').unwrap().unwrap();
        assert_eq!(Notification::BlockErr(b"%error not really\nhello"), n);
    }

    #[test]
    fn tmux_block_terminator_requires_exact_token_count() {
        let mut c = new_parser();
        put_all_none(&mut c, b"%begin 1 1 1\n");
        put_all_none(&mut c, b"%end 1 1 1 trailing\n");
        put_all_none(&mut c, b"hello\n");
        put_all_none(&mut c, b"%end 1 1 1");
        let n = c.put(b'\n').unwrap().unwrap();
        assert_eq!(Notification::BlockEnd(b"%end 1 1 1 trailing\nhello"), n);
    }

    #[test]
    fn tmux_block_terminator_requires_numeric_metadata() {
        let mut c = new_parser();
        put_all_none(&mut c, b"%begin 1 1 1\n");
        put_all_none(&mut c, b"%end foo bar baz\n");
        put_all_none(&mut c, b"hello\n");
        put_all_none(&mut c, b"%end 1 1 1");
        let n = c.put(b'\n').unwrap().unwrap();
        assert_eq!(Notification::BlockEnd(b"%end foo bar baz\nhello"), n);
    }

    #[test]
    fn tmux_output() {
        let mut c = new_parser();
        put_all_none(&mut c, b"%output %42 foo bar baz");
        let n = c.put(b'\n').unwrap().unwrap();
        let Notification::Output { pane_id, data } = n else {
            panic!("expected output, got {n:?}");
        };
        assert_eq!(42, pane_id);
        assert_eq!(b"foo bar baz", data);
    }

    #[test]
    fn tmux_session_changed() {
        let mut c = new_parser();
        put_all_none(&mut c, b"%session-changed $42 foo");
        let n = c.put(b'\n').unwrap().unwrap();
        let Notification::SessionChanged { id, name } = n else {
            panic!("expected session_changed, got {n:?}");
        };
        assert_eq!(42, id);
        assert_eq!(b"foo", name);
    }

    #[test]
    fn tmux_sessions_changed() {
        let mut c = new_parser();
        put_all_none(&mut c, b"%sessions-changed");
        let n = c.put(b'\n').unwrap().unwrap();
        assert_eq!(Notification::SessionsChanged, n);
    }

    #[test]
    fn tmux_sessions_changed_carriage_return() {
        let mut c = new_parser();
        put_all_none(&mut c, b"%sessions-changed\r");
        let n = c.put(b'\n').unwrap().unwrap();
        assert_eq!(Notification::SessionsChanged, n);
    }

    #[test]
    fn tmux_layout_change() {
        let mut c = new_parser();
        put_all_none(&mut c, b"%layout-change @2 1234x791,0,0{617x791,0,0,0,617x791,618,0,1} 1234x791,0,0{617x791,0,0,0,617x791,618,0,1} *-");
        let n = c.put(b'\n').unwrap().unwrap();
        let Notification::LayoutChange {
            window_id,
            layout,
            visible_layout,
            raw_flags,
        } = n
        else {
            panic!("expected layout_change, got {n:?}");
        };
        assert_eq!(2, window_id);
        assert_eq!(
            b"1234x791,0,0{617x791,0,0,0,617x791,618,0,1}".as_slice(),
            layout
        );
        assert_eq!(
            b"1234x791,0,0{617x791,0,0,0,617x791,618,0,1}".as_slice(),
            visible_layout
        );
        assert_eq!(b"*-", raw_flags);
    }

    #[test]
    fn tmux_window_add() {
        let mut c = new_parser();
        put_all_none(&mut c, b"%window-add @14");
        let n = c.put(b'\n').unwrap().unwrap();
        assert_eq!(Notification::WindowAdd { id: 14 }, n);
    }

    #[test]
    fn tmux_window_renamed() {
        let mut c = new_parser();
        put_all_none(&mut c, b"%window-renamed @42 bar");
        let n = c.put(b'\n').unwrap().unwrap();
        let Notification::WindowRenamed { id, name } = n else {
            panic!("expected window_renamed, got {n:?}");
        };
        assert_eq!(42, id);
        assert_eq!(b"bar", name);
    }

    #[test]
    fn tmux_window_pane_changed() {
        let mut c = new_parser();
        put_all_none(&mut c, b"%window-pane-changed @42 %2");
        let n = c.put(b'\n').unwrap().unwrap();
        let Notification::WindowPaneChanged { window_id, pane_id } = n else {
            panic!("expected window_pane_changed, got {n:?}");
        };
        assert_eq!(42, window_id);
        assert_eq!(2, pane_id);
    }

    #[test]
    fn tmux_client_detached() {
        let mut c = new_parser();
        put_all_none(&mut c, b"%client-detached /dev/pts/1");
        let n = c.put(b'\n').unwrap().unwrap();
        let Notification::ClientDetached { client } = n else {
            panic!("expected client_detached, got {n:?}");
        };
        assert_eq!(b"/dev/pts/1", client);
    }

    #[test]
    fn tmux_client_session_changed() {
        let mut c = new_parser();
        put_all_none(&mut c, b"%client-session-changed /dev/pts/1 $2 mysession");
        let n = c.put(b'\n').unwrap().unwrap();
        let Notification::ClientSessionChanged {
            client,
            session_id,
            name,
        } = n
        else {
            panic!("expected client_session_changed, got {n:?}");
        };
        assert_eq!(b"/dev/pts/1", client);
        assert_eq!(2, session_id);
        assert_eq!(b"mysession", name);
    }
}
