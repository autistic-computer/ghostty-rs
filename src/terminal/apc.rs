//! Port of `src/terminal/apc.zig`.
//!
//! STATUS: ported

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "apc/glyph.rs"]
pub mod glyph;
// @generated-mods-end

use crate::terminal::build_options;
use crate::terminal::kitty::graphics as kitty_gfx;

const LOG_TARGET: &str = "terminal_apc";

/// The glyph protocol identifier length (`glyph.identifier.len`).
const IDENTIFIER_LEN: usize = glyph::IDENTIFIER.len();

/// Maximum bytes per APC protocol (Zig: `std.EnumMap(Protocol, usize)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaxBytes {
    values: [Option<usize>; Protocol::COUNT],
}

impl MaxBytes {
    /// A map with only the given entries present (`EnumMap.init`).
    pub const fn init(kitty: Option<usize>, glyph: Option<usize>) -> MaxBytes {
        MaxBytes {
            values: [kitty, glyph],
        }
    }

    /// A map with every entry present (`EnumMap.initFullWith`).
    pub const fn init_full_with(kitty: usize, glyph: usize) -> MaxBytes {
        MaxBytes::init(Some(kitty), Some(glyph))
    }

    /// A map with every entry set to `value` (`EnumMap.initFull`).
    pub const fn init_full(value: usize) -> MaxBytes {
        MaxBytes::init_full_with(value, value)
    }

    pub fn get(&self, protocol: Protocol) -> Option<usize> {
        self.values[protocol as usize]
    }

    pub fn put(&mut self, protocol: Protocol, value: usize) {
        self.values[protocol as usize] = Some(value);
    }

    /// Remove the limit for `protocol` (Zig: `std.EnumMap.remove`).
    pub fn remove(&mut self, protocol: Protocol) {
        self.values[protocol as usize] = None;
    }
}

impl Default for MaxBytes {
    fn default() -> Self {
        MaxBytes::init_full_with(
            Protocol::Kitty.default_max_bytes(),
            Protocol::Glyph.default_max_bytes(),
        )
    }
}

/// A set of protocols (Zig: `std.EnumSet(Protocol)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolSet {
    bits: u8,
}

impl ProtocolSet {
    pub const EMPTY: ProtocolSet = ProtocolSet { bits: 0 };
    pub const FULL: ProtocolSet = ProtocolSet {
        bits: (1 << Protocol::COUNT) - 1,
    };

    pub fn contains(&self, protocol: Protocol) -> bool {
        self.bits & (1 << protocol as u8) != 0
    }

    pub fn set_present(&mut self, protocol: Protocol, present: bool) {
        if present {
            self.bits |= 1 << protocol as u8;
        } else {
            self.bits &= !(1 << protocol as u8);
        }
    }
}

impl Default for ProtocolSet {
    fn default() -> Self {
        ProtocolSet::FULL
    }
}

/// APC command handler. This should be hooked into a terminal.Stream handler.
/// The start/feed/end functions are meant to be called from the terminal.Stream
/// apcStart, apcPut, and apcEnd functions, respectively.
#[derive(Debug, Default)]
pub struct Handler {
    pub state: State,

    /// Maximum content bytes retained for unsupported APC identifiers. Zero
    /// drops and ignores unknown APC values.
    pub unknown_max_bytes: usize,

    /// Maximum bytes each APC protocol can buffer. This is to prevent
    /// malicious input from causing us to allocate too much memory.
    /// If you want to be lazy and set a single value for all protocols,
    /// use `MaxBytes::init_full`.
    pub max_bytes: MaxBytes,

    /// Protocols recognized by this APC handler. When a protocol is absent,
    /// matching APC sequences are ignored and are not reported as unknown.
    pub enabled: ProtocolSet,
}

impl Handler {
    pub fn deinit(&mut self) {
        self.state.deinit();
    }

    pub fn start(&mut self) {
        self.state.deinit();
        self.state = State::Identify {
            len: 0,
            buf: [0; IDENTIFIER_LEN],
        };
    }

    /// Enable or disable APC protocol recognition for future APC sequences.
    /// This does not affect any APC command already being parsed.
    pub fn enable(&mut self, protocol: Protocol, enabled: bool) {
        self.enabled.set_present(protocol, enabled);
    }

    pub fn feed(&mut self, byte: u8) {
        match &mut self.state {
            State::Inactive => unreachable!(),

            // We're ignoring this APC command, likely because we don't
            // recognize it so there is no need to store the data in memory.
            State::Ignore => {}

            // Unsupported APC content is retained only when enabled.
            State::Unknown(unknown) => unknown.append(&[byte]),

            // We identify the APC command by the first byte.
            State::Identify { len, buf } => 'id: {
                // Kitty graphics is detected immediately on the `G` byte,
                // since commands begin immediately after with no termination
                // character after the 'G'.
                if *len == 0 && byte == b'G' {
                    if build_options::KITTY_GRAPHICS && self.enabled.contains(Protocol::Kitty) {
                        self.state = State::Kitty(kitty_gfx::CommandParser::init(
                            self.max_bytes
                                .get(Protocol::Kitty)
                                .unwrap_or(Protocol::Kitty.default_max_bytes()),
                        ));
                    } else {
                        self.state = State::Ignore;
                    }
                    break 'id;
                }

                // If we hit `;` then identify...
                if byte == b';' {
                    let (len, buf) = (*len as usize, *buf);
                    let str = &buf[..len];
                    if str == glyph::IDENTIFIER.as_bytes() {
                        if build_options::GLYPH_PROTOCOL && self.enabled.contains(Protocol::Glyph) {
                            self.state = State::Glyph(glyph::CommandParser::init(
                                self.max_bytes
                                    .get(Protocol::Glyph)
                                    .unwrap_or(Protocol::Glyph.default_max_bytes()),
                            ));
                        } else {
                            self.state = State::Ignore;
                        }
                    } else {
                        self.begin_unknown(str, &[byte]);
                    }

                    break 'id;
                }

                // If we're out of identification space, the identifier is
                // unsupported. Preserve the buffered prefix before replacing
                // the identify union state.
                if *len as usize >= buf.len() {
                    let buf = *buf;
                    self.begin_unknown(&buf, &[byte]);
                    break 'id;
                }

                let expected_idx = *len as usize;
                buf[expected_idx] = byte;
                *len += 1;

                // Once the buffered input is no longer a prefix of a known
                // protocol, it is an unsupported identifier.
                if self.unknown_max_bytes > 0 && byte != glyph::IDENTIFIER.as_bytes()[expected_idx] {
                    let (len, buf) = (*len as usize, *buf);
                    self.begin_unknown(&buf[..len], &[]);
                }
            }

            State::Kitty(p) => {
                debug_assert!(build_options::KITTY_GRAPHICS);
                if let Err(err) = p.feed(byte) {
                    log::warn!(target: LOG_TARGET, "kitty graphics protocol error: {}", err);
                    self.state = State::Ignore;
                }
            }

            State::Glyph(p) => {
                debug_assert!(build_options::GLYPH_PROTOCOL);
                if let Err(err) = p.feed(byte) {
                    log::warn!(target: LOG_TARGET, "glyph protocol error: {}", err);
                    self.state = State::Ignore;
                }
            }
        }
    }

    /// Transition from protocol identification to bounded unknown capture.
    fn begin_unknown(&mut self, prefix: &[u8], suffix: &[u8]) {
        let max_bytes = self.unknown_max_bytes;
        if max_bytes == 0 {
            self.state = State::Ignore;
            return;
        }

        // Build the replacement before overwriting identify because prefix
        // points into that union field (callers pass a copy in Rust).
        let mut unknown = UnknownBuilder::init(max_bytes);
        unknown.append(prefix);
        unknown.append(suffix);
        self.state = State::Unknown(unknown);
    }

    /// Feed a slice of bytes to the handler. This is equivalent to
    /// calling feed for each byte in order, but protocol payload bytes
    /// are passed through in bulk so large payloads (e.g. Kitty graphics
    /// images) avoid per-byte dispatch overhead.
    pub fn feed_slice(&mut self, bytes: &[u8]) {
        let mut rem = bytes;
        while !rem.is_empty() {
            match &mut self.state {
                State::Inactive => unreachable!(),

                // We're ignoring this APC command; drop the whole slice.
                State::Ignore => return,

                // We're capturing an unknown APC command, so store it.
                State::Unknown(unknown) => {
                    unknown.append(rem);
                    return;
                }

                // Identification consumes at most a few bytes; step
                // through them one at a time until the state changes.
                State::Identify { .. } => {
                    self.feed(rem[0]);
                    rem = &rem[1..];
                }

                State::Kitty(p) => {
                    debug_assert!(build_options::KITTY_GRAPHICS);
                    if let Err(err) = p.feed_slice(rem) {
                        log::warn!(target: LOG_TARGET, "kitty graphics protocol error: {}", err);
                        self.state = State::Ignore;
                    }
                    return;
                }

                State::Glyph(p) => {
                    debug_assert!(build_options::GLYPH_PROTOCOL);
                    if let Err(err) = p.feed_slice(rem) {
                        log::warn!(target: LOG_TARGET, "glyph protocol error: {}", err);
                        self.state = State::Ignore;
                    }
                    return;
                }
            }
        }
    }

    /// Complete the current APC. The caller owns a returned result.
    pub fn end(&mut self) -> Option<Command> {
        let mut state = std::mem::replace(&mut self.state, State::Inactive);
        let result = match &mut state {
            State::Inactive => unreachable!(),
            State::Ignore | State::Identify { .. } => None,
            State::Unknown(unknown) => Some(Command::Unknown(unknown.to_owned())),
            State::Kitty(p) => {
                debug_assert!(build_options::KITTY_GRAPHICS);
                match p.complete() {
                    Ok(command) => Some(Command::Kitty(command)),
                    Err(err) => {
                        log::warn!(target: LOG_TARGET, "kitty graphics protocol error: {}", err);
                        None
                    }
                }
            }
            State::Glyph(p) => {
                debug_assert!(build_options::GLYPH_PROTOCOL);
                match p.complete() {
                    Ok(command) => Some(Command::Glyph(command)),
                    Err(err) => {
                        log::warn!(target: LOG_TARGET, "glyph protocol error: {}", err);
                        None
                    }
                }
            }
        };
        state.deinit();
        result
    }
}

#[derive(Debug, Default)]
pub enum State {
    /// We're not in the middle of an APC command yet.
    #[default]
    Inactive,

    /// We got an unrecognized APC sequence or the APC sequence we
    /// recognized became invalid. We're just dropping bytes.
    Ignore,

    /// We're waiting to identify the APC sequence. The way this is done
    /// is pretty fluid depending on supported APC protocols, but for now
    /// our rule is:
    ///
    ///  * 'G' - immediate transition to Kitty graphics protocol
    ///  * Buffer up to `;` and the bytes before dictate the protocol.
    ///    If we overflow then we're immediately invalid because we don't
    ///    support anything longer than this.
    ///
    Identify {
        /// A u3 in Zig.
        len: u8,
        buf: [u8; IDENTIFIER_LEN],
    },

    /// Kitty graphics protocol (only constructed when
    /// `build_options::KITTY_GRAPHICS`).
    Kitty(kitty_gfx::CommandParser),

    /// Glyph protocol (only constructed when
    /// `build_options::GLYPH_PROTOCOL`).
    Glyph(glyph::CommandParser),

    /// An unsupported APC retained for the optional unknown callback.
    /// Keep this after recognized protocol states so their tag values and
    /// generated dispatch stay stable when unknown capture is unused.
    Unknown(UnknownBuilder),
}

impl State {
    pub fn deinit(&mut self) {
        match self {
            State::Inactive | State::Ignore | State::Identify { .. } => {}
            State::Unknown(v) => v.deinit(),
            // Parser memory is released when the state is dropped.
            State::Glyph(_) | State::Kitty(_) => {}
        }
    }

    /// True when this is the `ignore` state (Zig `state == .ignore`).
    pub fn is_ignore(&self) -> bool {
        matches!(self, State::Ignore)
    }
}

/// UnknownBuilder is responsible for accumulating bytes for an
/// unidentified APC command if unknown capture is enabled.
#[derive(Debug)]
pub struct UnknownBuilder {
    data: Vec<u8>,
    max_bytes: usize,
    truncated: bool,
}

impl UnknownBuilder {
    fn init(max_bytes: usize) -> UnknownBuilder {
        UnknownBuilder {
            data: Vec::new(),
            max_bytes,
            truncated: false,
        }
    }

    fn deinit(&mut self) {
        self.data = Vec::new();
    }

    // Append some bytes to the unknown capture. This flags as truncated
    // if allocation fails or we reach our byte limit, therefore
    // it can't fail.
    fn append(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let current = self.data.len();

        // Determine how many bytes we can store in this append and
        // if it is less than our input, then we have to note we're
        // truncating.
        let retained = bytes.len().min(self.max_bytes.saturating_sub(current));
        if retained < bytes.len() {
            self.truncated = true;
        }

        // If we require more bytes than our capacity allows then we
        // need to grow.
        let required = current + retained;
        if required > self.data.capacity() {
            let capacity = self
                .max_bytes
                .min(required.max(self.data.capacity().saturating_mul(2).max(1)));
            if self.data.try_reserve_exact(capacity - current).is_err() {
                self.truncated = true;
                return;
            }
        }

        self.data.extend_from_slice(&bytes[..retained]);
    }

    /// Convert the current capture state to an Unknown where ownership
    /// shifts to Unknown. Removes any accumulated unknown capture in this
    /// struct.
    fn to_owned(&mut self) -> Unknown {
        let mut content = std::mem::take(&mut self.data);
        content.shrink_to_fit();
        Unknown {
            content,
            truncated: self.truncated,
        }
    }
}

/// An unsupported APC returned by `Handler::end`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unknown {
    pub content: Vec<u8>,
    pub truncated: bool,
}

/// Possible APC command types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Protocol {
    Kitty = 0,
    Glyph = 1,
}

impl Protocol {
    const COUNT: usize = 2;

    /// All protocols (`std.enums.values(Protocol)`).
    pub const ALL: [Protocol; Protocol::COUNT] = [Protocol::Kitty, Protocol::Glyph];

    /// Returns the default maximum bytes for the given protocol.
    pub const fn default_max_bytes(self) -> usize {
        match self {
            // Kitty graphics payloads can be very large (e.g. full images
            // encoded as base64), so the default is set to 65 MiB.
            Protocol::Kitty => 65 * 1024 * 1024,
            // Glyph protocol messages carry single glyf outlines which
            // are small, but base64 encoding inflates them. 1 MiB is
            // generous for any single simple-glyph record.
            Protocol::Glyph => 1024 * 1024,
        }
    }

    /// Return the largest default buffer limit across every APC protocol.
    /// Consumers that must retain any unfinished APC can derive their limit
    /// here instead of duplicating a particular protocol's current default.
    pub fn max_default_bytes() -> usize {
        let mut result = 0;
        for protocol in Protocol::ALL {
            result = result.max(protocol.default_max_bytes());
        }
        result
    }
}

/// A recognized or unsupported APC command.
#[derive(Debug)]
pub enum Command {
    /// Only produced when `build_options::KITTY_GRAPHICS`.
    Kitty(kitty_gfx::Command),

    /// Only produced when `build_options::GLYPH_PROTOCOL`.
    Glyph(glyph::Request),

    Unknown(Unknown),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_all(h: &mut Handler, bytes: &[u8]) {
        for &c in bytes {
            h.feed(c);
        }
    }

    #[test]
    fn unknown_apc_command() {
        let mut h = Handler::default();
        h.start();
        feed_all(&mut h, b"Xabcdef1234");
        assert!(h.end().is_none());
    }

    #[test]
    fn capture_unknown_apc_command() {
        let mut h = Handler {
            unknown_max_bytes: 5,
            ..Default::default()
        };
        h.start();
        h.feed_slice(b"abcd;payload");

        let result = h.end().unwrap();
        let Command::Unknown(unknown) = &result else {
            panic!("expected unknown");
        };
        assert_eq!(b"abcd;", unknown.content.as_slice());
        assert!(unknown.truncated);
        h.deinit();
    }

    #[test]
    fn capture_short_unknown_apc_command() {
        let mut h = Handler {
            unknown_max_bytes: 16,
            ..Default::default()
        };
        h.start();
        h.feed(b'X');

        let result = h.end().unwrap();
        let Command::Unknown(unknown) = &result else {
            panic!("expected unknown");
        };
        assert_eq!(b"X", unknown.content.as_slice());
        assert!(!unknown.truncated);

        h.unknown_max_bytes = 1;
        h.start();
        h.feed_slice(b"XYZ");
        let result = h.end().unwrap();
        let Command::Unknown(truncated) = &result else {
            panic!("expected unknown");
        };
        assert_eq!(b"X", truncated.content.as_slice());
        assert!(truncated.truncated);
        h.deinit();
    }

    #[test]
    fn disabled_known_apc_protocol_is_not_unknown() {
        let mut h = Handler {
            unknown_max_bytes: 64,
            ..Default::default()
        };
        h.enable(Protocol::Glyph, false);
        h.start();
        h.feed_slice(b"25a1;q;cp=E0A0");
        assert!(h.end().is_none());

        // An incomplete known protocol identifier is malformed, not unknown.
        h.start();
        h.feed_slice(b"25a");
        assert!(h.end().is_none());
        h.deinit();
    }

    #[test]
    fn garbage_kitty_command() {
        if !build_options::KITTY_GRAPHICS {
            return;
        }

        let mut h = Handler::default();
        h.start();
        feed_all(&mut h, b"Gabcdef1234");
        assert!(h.end().is_none());
    }

    #[test]
    fn kitty_command_with_overflow_u32() {
        if !build_options::KITTY_GRAPHICS {
            return;
        }

        let mut h = Handler::default();
        h.start();
        feed_all(&mut h, b"Ga=p,i=10000000000");
        assert!(h.end().is_none());
    }

    #[test]
    fn kitty_command_with_overflow_i32() {
        if !build_options::KITTY_GRAPHICS {
            return;
        }

        let mut h = Handler::default();
        h.start();
        feed_all(&mut h, b"Ga=p,i=1,z=-9999999999");
        assert!(h.end().is_none());
    }

    #[test]
    fn kitty_feed_error_deinits_parser() {
        if !build_options::KITTY_GRAPHICS {
            return;
        }

        // Feed a valid kitty command start to allocate parser state, then
        // trigger an error during feed via an integer overflow.
        let mut h = Handler::default();
        h.start();
        feed_all(&mut h, b"Ga=p,i=10000000000;");
        assert!(h.state.is_ignore());
        h.deinit();
    }

    #[test]
    fn kitty_max_bytes_exceeded() {
        if !build_options::KITTY_GRAPHICS {
            return;
        }

        let mut h = Handler {
            max_bytes: MaxBytes::init(Some(4), None),
            ..Default::default()
        };
        h.start();
        // 'G' identifies kitty, 'a=t;' moves to data state, then feed exceeds max_bytes.
        feed_all(&mut h, b"Ga=t;");
        assert!(!h.state.is_ignore());
        feed_all(&mut h, b"abcd");
        assert!(!h.state.is_ignore());
        // The 5th data byte exceeds the 4-byte limit.
        h.feed(b'e');
        assert!(h.state.is_ignore());
        h.deinit();
    }

    #[test]
    fn valid_kitty_command() {
        if !build_options::KITTY_GRAPHICS {
            return;
        }

        let mut h = Handler::default();
        h.start();
        feed_all(&mut h, b"Gf=24,s=10,v=20,hello=world");

        let result = h.end().unwrap();
        assert!(matches!(result, Command::Kitty(_)));
    }

    #[test]
    fn identify_with_unrecognized_command() {
        let mut h = Handler::default();
        h.start();
        feed_all(&mut h, b"abcd;payload");
        assert!(h.end().is_none());
    }

    #[test]
    fn identify_buffer_overflow() {
        let mut h = Handler::default();
        h.start();
        feed_all(&mut h, b"abcde;payload");
        assert!(h.end().is_none());
    }

    #[test]
    fn identify_with_no_input() {
        let mut h = Handler::default();
        h.start();
        assert!(h.end().is_none());
    }

    #[test]
    fn identify_with_unknown_partial_input() {
        let mut h = Handler::default();
        h.start();
        feed_all(&mut h, b"25a");
        assert!(h.end().is_none());
    }

    #[test]
    fn garbage_glyph_command() {
        let mut h = Handler::default();
        h.start();
        feed_all(&mut h, b"25a1;X");

        assert!(h.end().is_none());
    }

    #[test]
    fn valid_glyph_command() {
        if !build_options::GLYPH_PROTOCOL {
            return;
        }

        let mut h = Handler::default();
        h.start();
        feed_all(&mut h, b"25a1;q;cp=E0A0");

        let result = h.end().unwrap();
        assert!(matches!(result, Command::Glyph(glyph::Request::Query(_))));
    }

    #[test]
    fn feed_slice_valid_kitty_command() {
        if !build_options::KITTY_GRAPHICS {
            return;
        }

        let mut h = Handler::default();
        h.start();
        h.feed_slice(b"Gf=24,s=10,v=20;aGVsbG8=");

        let result = h.end().unwrap();
        let Command::Kitty(cmd) = &result else {
            panic!("expected kitty");
        };

        // The payload is base64-decoded by the parser on completion.
        assert_eq!(b"hello", cmd.data.as_slice());
    }

    #[test]
    fn feed_slice_identify_split_across_slices() {
        if !build_options::KITTY_GRAPHICS {
            return;
        }

        let mut h = Handler::default();
        h.start();
        h.feed_slice(b"G");
        h.feed_slice(b"f=24,s=10,");
        h.feed_slice(b"v=20;aGVsbG8=");

        let result = h.end().unwrap();
        let Command::Kitty(cmd) = &result else {
            panic!("expected kitty");
        };

        // The payload is base64-decoded by the parser on completion.
        assert_eq!(b"hello", cmd.data.as_slice());
    }

    #[test]
    fn feed_slice_unknown_apc_command_is_ignored() {
        let mut h = Handler::default();
        h.start();
        h.feed_slice(b"Xabcdef1234");
        assert!(h.state.is_ignore());
        h.feed_slice(b"more data that is dropped");
        assert!(h.end().is_none());
    }

    #[test]
    fn feed_slice_valid_glyph_command() {
        if !build_options::GLYPH_PROTOCOL {
            return;
        }

        let mut h = Handler::default();
        h.start();
        h.feed_slice(b"25a1;q;cp=E0A0");

        let result = h.end().unwrap();
        assert!(matches!(result, Command::Glyph(glyph::Request::Query(_))));
    }

    #[test]
    fn feed_slice_kitty_max_bytes_exceeded() {
        if !build_options::KITTY_GRAPHICS {
            return;
        }

        let mut h = Handler {
            max_bytes: MaxBytes::init(Some(4), None),
            ..Default::default()
        };
        h.start();
        h.feed_slice(b"Ga=t;abcd");
        assert!(!h.state.is_ignore());
        h.feed_slice(b"e");
        assert!(h.state.is_ignore());
        h.deinit();
    }

    #[test]
    fn disabled_glyph_command_is_ignored() {
        let mut h = Handler::default();
        h.enable(Protocol::Glyph, false);
        h.start();
        feed_all(&mut h, b"25a1;q;cp=e0a0");
        assert!(h.end().is_none());
    }
}
