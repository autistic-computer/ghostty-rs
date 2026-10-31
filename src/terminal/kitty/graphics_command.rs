//! Port of `src/terminal/kitty/graphics_command.zig`.
//!
//! STATUS: ported

use std::fmt;
use std::io;

use crate::simd::base64;

// The `kitty_gfx` log scope.
const LOG_TARGET: &str = "kitty_gfx";

/// Errors returned while parsing a graphics command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    /// The command is structurally invalid or has an unknown flag value.
    InvalidFormat,
    /// The base64 payload could not be decoded.
    InvalidData,
    /// The payload exceeded the parser's `max_bytes`.
    OutOfMemory,
    /// `std.fmt.parseInt`: the value doesn't fit the key's integer type.
    Overflow,
    /// `std.fmt.parseInt`: the value isn't a valid integer.
    InvalidCharacter,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for ParseError {}

/// The key-value pairs for the control information for a command. The
/// recognized keys are always single ASCII letters and the values are either
/// single characters or 32-bit unsigned integers. Unknown non-letter keys are
/// ignored, matching how unknown letter keys are ignored when building a
/// command.
///
/// This is deliberately a dense value table plus a presence bitmap. There are
/// only 52 possible keys, so a lookup is one machine-word bit test followed by
/// an indexed load. The values are meaningless until their presence bit is set.
///
/// For the value itself: if it is a single printable ASCII character it is the
/// ASCII code. Otherwise, it is parsed as a 32-bit unsigned integer.
#[derive(Debug, Clone, Copy)]
struct Kv {
    values: [u32; 52],
    present: u64,
}

impl Default for Kv {
    fn default() -> Self {
        Kv {
            values: [0; 52],
            present: 0,
        }
    }
}

impl Kv {
    fn index(key: u8) -> Option<u32> {
        match key {
            b'a'..=b'z' => Some(u32::from(key - b'a')),
            b'A'..=b'Z' => Some(26 + u32::from(key - b'A')),
            _ => None,
        }
    }

    fn get(&self, key: u8) -> Option<u32> {
        let idx = Self::index(key)?;
        if self.present & (1u64 << idx) == 0 {
            return None;
        }
        Some(self.values[idx as usize])
    }

    fn put(&mut self, key: u8, value: u32) {
        let Some(idx) = Self::index(key) else { return };
        self.values[idx as usize] = value;
        self.present |= 1u64 << idx;
    }
}

/// Internal state for parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Parsing k/v pairs. The "ignore" variants are in that state
    /// but ignore any data because we know they're invalid.
    ControlKey,
    ControlKeyIgnore,
    ControlValue,
    ControlValueIgnore,

    /// Collecting the data payload blob.
    Data,
}

/// Command parser parses the Kitty graphics protocol escape sequence.
#[derive(Debug, Clone)]
pub struct Parser {
    /// This is the list of KV pairs that we're building up.
    kv: Kv,

    /// This is used as a buffer to store the key/value of a KV pair. The value
    /// of a KV pair is at most a 32-bit integer which at most is 10 characters
    /// (4294967295), plus one character for the sign bit on signed ints.
    kv_temp: [u8; 11],
    kv_temp_len: u8,
    kv_current: u8, // Current kv key

    /// This is the list we use to collect the bytes from the data payload.
    /// The Kitty Graphics protocol specification seems to imply that the
    /// payload content of a single command should never exceed 4096 bytes,
    /// but Kitty itself supports larger payloads, so we use a Vec
    /// here instead of a fixed buffer so that we can too.
    data: Vec<u8>,

    /// Maximum bytes the data payload can take. This is to prevent
    /// malicious input from causing us to allocate too much memory.
    max_bytes: usize,

    /// Internal state for parsing.
    state: State,
}

impl Parser {
    /// Initialize the parser.
    pub fn init(max_bytes: usize) -> Parser {
        Parser {
            kv: Kv::default(),
            kv_temp: [0; 11],
            kv_temp_len: 0,
            kv_current: 0,
            data: Vec::new(),
            max_bytes,
            state: State::ControlKey,
        }
    }

    /// Parse a complete command string.
    pub fn parse_string(data: &[u8]) -> Result<Command, ParseError> {
        let mut parser = Parser::init(1024 * 1024);
        for &c in data {
            parser.feed(c)?;
        }
        parser.complete()
    }

    /// Feed a single byte to the parser.
    ///
    /// The first byte to start parsing should be the byte immediately following
    /// the "G" in the APC sequence, i.e. "\x1b_G123" the first byte should
    /// be "1".
    pub fn feed(&mut self, c: u8) -> Result<(), ParseError> {
        match self.state {
            State::ControlKey => match c {
                // '=' means the key is complete and we're moving to the value.
                b'=' => {
                    if self.kv_temp_len != 1 {
                        // All control keys are a single character right now so
                        // if we're not a single character just ignore follow-up
                        // data.
                        self.state = State::ControlValueIgnore;
                        self.kv_temp_len = 0;
                    } else {
                        self.kv_current = self.kv_temp[0];
                        self.kv_temp_len = 0;
                        self.state = State::ControlValue;
                    }
                }

                // This can be encountered if we have a sequence with no
                // control data, only payload data (i.e. "\x1b_G;<data>").
                //
                // Kitty treats this as valid so we do as well.
                b';' => self.state = State::Data,

                _ => self.accumulate_value(c, State::ControlKeyIgnore),
            },

            State::ControlKeyIgnore => {
                if c == b'=' {
                    self.state = State::ControlValueIgnore;
                }
            }

            State::ControlValue => match c {
                b',' => self.finish_value(State::ControlKey)?, // move to next key
                b';' => self.finish_value(State::Data)?,       // move to data
                _ => self.accumulate_value(c, State::ControlValueIgnore),
            },

            State::ControlValueIgnore => match c {
                b',' => self.state = State::ControlKeyIgnore,
                b';' => self.state = State::Data,
                _ => {}
            },

            State::Data => {
                if self.data.len() >= self.max_bytes {
                    return Err(ParseError::OutOfMemory);
                }
                self.data.push(c);
            }
        }
        Ok(())
    }

    /// Feed a slice of bytes to the parser. This is equivalent to
    /// calling feed for each byte in order, but once we're in the data
    /// state the remainder of the slice is appended in bulk, avoiding
    /// per-byte overhead for large payloads.
    pub fn feed_slice(&mut self, bytes: &[u8]) -> Result<(), ParseError> {
        let mut rem = bytes;
        while !rem.is_empty() {
            if self.state == State::Data {
                if self.data.len() + rem.len() > self.max_bytes {
                    return Err(ParseError::OutOfMemory);
                }
                self.data.extend_from_slice(rem);
                return Ok(());
            }

            self.feed(rem[0])?;
            rem = &rem[1..];
        }
        Ok(())
    }

    /// Complete the parsing. This must be called after all the
    /// bytes have been fed to the parser.
    pub fn complete(&mut self) -> Result<Command, ParseError> {
        match self.state {
            // We can't ever end in the control key state and be valid.
            // This means the command looked something like "a=1,b"
            State::ControlKey | State::ControlKeyIgnore => return Err(ParseError::InvalidFormat),

            // Some commands (i.e. placements) end without extra data so
            // we end in the value state. i.e. "a=1,b=2"
            State::ControlValue => self.finish_value(State::Data)?,
            State::ControlValueIgnore => {}

            // Most commands end in data, i.e. "a=1,b=2;1234"
            State::Data => {}
        }

        // Determine our action, which is always a single character.
        let action: u8 = match self.kv.get(b'a') {
            None => b't',
            Some(value) => u8::try_from(value).map_err(|_| ParseError::InvalidFormat)?,
        };
        let control: Control = match action {
            b'q' => Control::Query(Transmission::parse(&self.kv)?),
            b't' => Control::Transmit(Transmission::parse(&self.kv)?),
            b'T' => Control::TransmitAndDisplay(TransmitAndDisplay {
                transmission: Transmission::parse(&self.kv)?,
                display: Display::parse(&self.kv),
            }),
            b'p' => Control::Display(Display::parse(&self.kv)),
            b'd' => Control::Delete(Delete::parse(&self.kv)?),
            b'f' => Control::TransmitAnimationFrame(AnimationFrameLoading::parse(&self.kv)?),
            b'a' => Control::ControlAnimation(AnimationControl::parse(&self.kv)),
            b'c' => Control::ComposeAnimation(AnimationFrameComposition::parse(&self.kv)),
            _ => return Err(ParseError::InvalidFormat),
        };

        // Determine our quiet value. The spec specifies 0 and 1 but Kitty
        // allows anything greater than 1 to suppress.
        let quiet = match self.kv.get(b'q') {
            Some(0) => Quiet::No,
            Some(1) => Quiet::Ok,
            Some(_) => Quiet::Failures,
            None => Quiet::No,
        };

        Ok(Command {
            control,
            quiet,
            data: self.decode_data()?,
        })
    }

    /// Decodes the payload data from base64 and returns it.
    /// This function will destroy the contents of self.data, it should
    /// only be used once we are done collecting payload bytes.
    fn decode_data(&mut self) -> Result<Vec<u8>, ParseError> {
        if self.data.is_empty() {
            return Ok(Vec::new());
        }

        let max_len = base64::max_len(&self.data);
        debug_assert!(max_len <= self.data.len());

        // The Zig version decodes the base64 on top of itself. Rust can't
        // alias the input and output, so decode into a fresh buffer.
        let mut decoded_buf = vec![0u8; max_len];
        let decoded_len = match base64::decode(&self.data, &mut decoded_buf) {
            Ok(decoded) => decoded.len(),
            Err(err) => {
                log::warn!(target: LOG_TARGET, "failed to decode base64 payload data: {err}");
                return Err(ParseError::InvalidData);
            }
        };
        debug_assert!(decoded_len <= max_len);

        // Remove the extra bytes.
        decoded_buf.truncate(decoded_len);
        self.data = Vec::new();

        Ok(decoded_buf)
    }

    fn accumulate_value(&mut self, c: u8, overflow_state: State) {
        let idx = self.kv_temp_len as usize;
        self.kv_temp_len += 1;
        if self.kv_temp_len as usize > self.kv_temp.len() {
            self.state = overflow_state;
            self.kv_temp_len = 0;
            return;
        }
        self.kv_temp[idx] = c;
    }

    fn finish_value(&mut self, next_state: State) -> Result<(), ParseError> {
        // We can move states right away, we don't use it.
        self.state = next_state;

        // Check for ASCII chars first
        if self.kv_temp_len == 1 {
            let c = self.kv_temp[0];
            if !c.is_ascii_digit() {
                self.kv.put(self.kv_current, u32::from(c));
                self.kv_temp_len = 0;
                return Ok(());
            }
        }

        // Handle integer fields, parsing signed fields accordingly. We still
        // store the fields as u32 as they can be bitcast back later during
        // building of the higher-level command tree.
        let buf = &self.kv_temp[..self.kv_temp_len as usize];
        let v: u32 = match self.kv_current {
            b'z' | b'H' | b'V' => {
                let v = parse_int(buf, i64::from(i32::MIN), i64::from(i32::MAX))?;
                (v as i32) as u32
            }
            _ => parse_int(buf, 0, i64::from(u32::MAX))? as u32,
        };
        self.kv.put(self.kv_current, v);

        // Clear our temp buffer
        self.kv_temp_len = 0;
        Ok(())
    }
}

/// `std.fmt.parseInt(T, buf, 10)` for an integer type with the range
/// `min..=max`: an optional leading `+` or `-`, `_` digit separators allowed
/// between digits, `InvalidCharacter` for anything else (including an empty
/// string) and `Overflow` when the value doesn't fit.
fn parse_int(buf: &[u8], min: i64, max: i64) -> Result<i64, ParseError> {
    if buf.is_empty() {
        return Err(ParseError::InvalidCharacter);
    }
    let (neg, digits) = match buf[0] {
        b'+' => (false, &buf[1..]),
        b'-' => (true, &buf[1..]),
        _ => (false, buf),
    };
    if digits.is_empty() || digits[0] == b'_' || digits[digits.len() - 1] == b'_' {
        return Err(ParseError::InvalidCharacter);
    }

    let mut x: i64 = 0;
    for &c in digits {
        if c == b'_' {
            continue;
        }
        let digit = match c {
            b'0'..=b'9' => i64::from(c - b'0'),
            _ => return Err(ParseError::InvalidCharacter),
        };
        x = x.checked_mul(10).ok_or(ParseError::Overflow)?;
        x = if neg {
            x.checked_sub(digit).ok_or(ParseError::Overflow)?
        } else {
            x.checked_add(digit).ok_or(ParseError::Overflow)?
        };
        if x < min || x > max {
            return Err(ParseError::Overflow);
        }
    }
    Ok(x)
}

/// Represents a possible response to a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Response {
    pub id: u32,
    pub image_number: u32,
    pub placement_id: u32,
    /// The 1-based animation frame number, echoed as "r=" for the
    /// animation frame actions. This is how clients learn the frame
    /// number assigned to a newly created frame.
    pub frame: u32,
    pub message: &'static str,
}

impl Default for Response {
    fn default() -> Self {
        Response {
            id: 0,
            image_number: 0,
            placement_id: 0,
            frame: 0,
            message: "OK",
        }
    }
}

impl Response {
    pub fn encode<W: io::Write + ?Sized>(&self, writer: &mut W) -> io::Result<()> {
        // We only encode a result if we have either an id or an image number.
        if self.id == 0 && self.image_number == 0 {
            return Ok(());
        }

        // Used to cheaply keep track if we need to add a comma before
        // the next key-value pair.
        let mut prior = false;

        writer.write_all(b"\x1b_G")?;
        if self.id > 0 {
            prior = true;
            write!(writer, "i={}", self.id)?;
        }
        if self.image_number > 0 {
            if prior {
                writer.write_all(b",")?;
            } else {
                prior = true;
            }
            write!(writer, "I={}", self.image_number)?;
        }
        if self.placement_id > 0 {
            if prior {
                writer.write_all(b",")?;
            } else {
                prior = true;
            }
            write!(writer, "p={}", self.placement_id)?;
        }
        if self.frame > 0 {
            if prior {
                writer.write_all(b",")?;
            }
            write!(writer, "r={}", self.frame)?;
        }
        writer.write_all(b";")?;
        writer.write_all(self.message.as_bytes())?;
        writer.write_all(b"\x1b\\")?;
        Ok(())
    }

    /// Returns true if this response is not an error.
    pub fn ok(&self) -> bool {
        self.message == "OK"
    }

    /// Empty response
    pub fn empty(&self) -> bool {
        self.id == 0 && self.image_number == 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub control: Control,
    pub quiet: Quiet,
    pub data: Vec<u8>,
}

/// `Command.Action`: the tag of [`Control`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Query,                  // q
    Transmit,               // t
    TransmitAndDisplay,     // T
    Display,                // p
    Delete,                 // d
    TransmitAnimationFrame, // f
    ControlAnimation,       // a
    ComposeAnimation,       // c
}

/// `Command.Quiet`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Quiet {
    #[default]
    No, // 0
    Ok,       // 1
    Failures, // 2
}

/// `Command.Control`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Query(Transmission),
    Transmit(Transmission),
    TransmitAndDisplay(TransmitAndDisplay),
    Display(Display),
    Delete(Delete),
    TransmitAnimationFrame(AnimationFrameLoading),
    ControlAnimation(AnimationControl),
    ComposeAnimation(AnimationFrameComposition),
}

/// The payload of [`Control::TransmitAndDisplay`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TransmitAndDisplay {
    pub transmission: Transmission,
    pub display: Display,
}

/// `Command.Control.Identifiers`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Identifiers {
    pub image_id: u32,
    pub image_number: u32,
    pub placement_id: u32,
}

impl Control {
    /// The action (union tag) of this control.
    pub fn action(&self) -> Action {
        match self {
            Control::Query(_) => Action::Query,
            Control::Transmit(_) => Action::Transmit,
            Control::TransmitAndDisplay(_) => Action::TransmitAndDisplay,
            Control::Display(_) => Action::Display,
            Control::Delete(_) => Action::Delete,
            Control::TransmitAnimationFrame(_) => Action::TransmitAnimationFrame,
            Control::ControlAnimation(_) => Action::ControlAnimation,
            Control::ComposeAnimation(_) => Action::ComposeAnimation,
        }
    }

    /// Returns the image and placement identifiers for any action.
    pub fn identifiers(&self) -> Identifiers {
        match self {
            Control::Query(t) | Control::Transmit(t) => Identifiers {
                image_id: t.image_id,
                image_number: t.image_number,
                placement_id: t.placement_id,
            },
            Control::TransmitAndDisplay(t) => Identifiers {
                image_id: t.transmission.image_id,
                image_number: t.transmission.image_number,
                placement_id: t.transmission.placement_id,
            },
            Control::Display(d) => Identifiers {
                image_id: d.image_id,
                image_number: d.image_number,
                placement_id: d.placement_id,
            },
            Control::Delete(d) => Identifiers {
                image_id: d.image_id,
                image_number: d.image_number,
                placement_id: d.placement_id,
            },
            Control::TransmitAnimationFrame(f) => Identifiers {
                image_id: f.transmission.image_id,
                image_number: f.transmission.image_number,
                placement_id: f.transmission.placement_id,
            },
            Control::ControlAnimation(a) => Identifiers {
                image_id: a.image_id,
                image_number: a.image_number,
                placement_id: a.placement_id,
            },
            Control::ComposeAnimation(c) => Identifiers {
                image_id: c.image_id,
                image_number: c.image_number,
                placement_id: c.placement_id,
            },
        }
    }
}

impl Command {
    /// Take ownership over the data in this command. If the returned value
    /// has a length of zero, then the data was empty.
    pub fn to_owned_data(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.data)
    }

    /// Returns the transmission data if it has any.
    pub fn transmission(&self) -> Option<Transmission> {
        match &self.control {
            Control::Query(t) => Some(*t),
            Control::Transmit(t) => Some(*t),
            Control::TransmitAndDisplay(t) => Some(t.transmission),
            Control::TransmitAnimationFrame(f) => Some(f.transmission),
            _ => None,
        }
    }

    /// Returns the display data if it has any.
    pub fn display(&self) -> Option<Display> {
        match &self.control {
            Control::Display(d) => Some(*d),
            Control::TransmitAndDisplay(t) => Some(t.display),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transmission {
    pub format: Format, // f
    /// Set when "f" carried a value we don't recognize. Kitty rejects these
    /// while handling the command so the EINVAL response can carry the image
    /// id, so we defer to `LoadingImage.init`. `format` is meaningless when
    /// this is set.
    pub format_unknown: bool,
    pub medium: Medium,           // t
    pub width: u32,               // s
    pub height: u32,              // v
    pub size: u32,                // S
    pub offset: u32,              // O
    pub image_id: u32,            // i
    pub image_number: u32,        // I
    pub placement_id: u32,        // p
    pub compression: Compression, // o
    pub more_chunks: bool,        // m
    pub usage: Usage,             // N
}

impl Default for Transmission {
    fn default() -> Self {
        Transmission {
            format: Format::Rgba,
            format_unknown: false,
            medium: Medium::Direct,
            width: 0,
            height: 0,
            size: 0,
            offset: 0,
            image_id: 0,
            image_number: 0,
            placement_id: 0,
            compression: Compression::None,
            more_chunks: false,
            usage: Usage::DEFAULT,
        }
    }
}

/// `Transmission.Format` (`lib.Enum`; discriminants are the key indices).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Format {
    Rgb = 0,  // 24
    Rgba = 1, // 32
    Png = 2,  // 100
    // The following are not supported directly via the protocol
    // but they are formats that a png may decode to that we
    // support.
    GrayAlpha = 3,
    Gray = 4,
}

/// `Transmission.Medium` (`lib.Enum`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Medium {
    Direct = 0,        // d
    File = 1,          // f
    TemporaryFile = 2, // t
    SharedMemory = 3,  // s
}

/// `Transmission.Compression` (`lib.Enum`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Compression {
    None = 0,
    ZlibDeflate = 1, // z
}

/// Usage hints allow for optimising resource consumption strategies.
///
/// https://sw.kovidgoyal.net/kitty/graphics-protocol/#usage-hints
///
/// `packed struct(u32)`: bit 0 is `transient`, bits 1..32 are padding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Usage {
    /// Image with this usage hint is assumed to be used for only a short
    /// time, so may be evicted before other images if memory pressure is
    /// encountered.
    pub transient: bool,
    /// The remaining 31 bits.
    pub _padding: u32,
}

impl Usage {
    pub const DEFAULT: Usage = Usage {
        transient: false,
        _padding: 0,
    };

    /// `@bitCast(u32)`
    pub const fn from_bits(v: u32) -> Usage {
        Usage {
            transient: v & 1 != 0,
            _padding: v >> 1,
        }
    }

    /// `@bitCast` back to the `u32` representation.
    pub const fn to_bits(self) -> u32 {
        (self._padding << 1) | self.transient as u32
    }
}

impl Transmission {
    pub fn format_bpp(format: Format) -> u8 {
        match format {
            Format::Gray => 1,
            Format::GrayAlpha => 2,
            Format::Rgb => 3,
            Format::Rgba => 4,
            Format::Png => unreachable!(), // Must be validated before
        }
    }

    fn parse(kv: &Kv) -> Result<Transmission, ParseError> {
        let mut result = Transmission::default();
        if let Some(v) = kv.get(b'f') {
            result.format = match v {
                24 => Format::Rgb,
                0 | 32 => Format::Rgba,
                100 => Format::Png,
                _ => {
                    // Defer returning an error until the image is processed.
                    result.format_unknown = true;
                    Format::Rgba
                }
            };
        }

        if let Some(v) = kv.get(b't') {
            let c = u8::try_from(v).map_err(|_| ParseError::InvalidFormat)?;
            result.medium = match c {
                b'd' => Medium::Direct,
                b'f' => Medium::File,
                b't' => Medium::TemporaryFile,
                b's' => Medium::SharedMemory,
                _ => return Err(ParseError::InvalidFormat),
            };
        }

        if let Some(v) = kv.get(b's') {
            result.width = v;
        }

        if let Some(v) = kv.get(b'v') {
            result.height = v;
        }

        if let Some(v) = kv.get(b'S') {
            result.size = v;
        }

        if let Some(v) = kv.get(b'O') {
            result.offset = v;
        }

        if let Some(v) = kv.get(b'i') {
            result.image_id = v;
        }

        if let Some(v) = kv.get(b'I') {
            result.image_number = v;
        }

        if let Some(v) = kv.get(b'p') {
            result.placement_id = v;
        }

        if let Some(v) = kv.get(b'o') {
            let c = u8::try_from(v).map_err(|_| ParseError::InvalidFormat)?;
            result.compression = match c {
                b'z' => Compression::ZlibDeflate,
                _ => return Err(ParseError::InvalidFormat),
            };
        }

        // If the transmission medium is a local-only medium, ignore the "m"
        // key. The Kitty graphics protocol specification does not explicitly
        // call out this behavior (although the "m" key is only mentioned in
        // connection with remote clients) but that's how it's implemented in
        // Kitty and at least one client (mpv) relies on this behavior when
        // using the shared memory transmission medium.
        //
        // https://sw.kovidgoyal.net/kitty/graphics-protocol/#the-transmission-medium
        // https://github.com/kovidgoyal/kitty/blob/ccc3bee9af794f332b4e9adcd714a649f639c397/kitty/graphics.c#L547-L592
        if result.medium == Medium::Direct {
            if let Some(v) = kv.get(b'm') {
                result.more_chunks = v > 0;
            }
        }

        if let Some(v) = kv.get(b'N') {
            result.usage = Usage::from_bits(v);
        }

        Ok(result)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Display {
    pub image_id: u32,                   // i
    pub image_number: u32,               // I
    pub placement_id: u32,               // p
    pub x: u32,                          // x
    pub y: u32,                          // y
    pub width: u32,                      // w
    pub height: u32,                     // h
    pub x_offset: u32,                   // X
    pub y_offset: u32,                   // Y
    pub columns: u32,                    // c
    pub rows: u32,                       // r
    pub cursor_movement: CursorMovement, // C
    pub virtual_placement: bool,         // U
    pub parent_id: u32,                  // P
    pub parent_placement_id: u32,        // Q
    pub horizontal_offset: i32,          // H
    pub vertical_offset: i32,            // V
    pub z: i32,                          // z
}

/// `Display.CursorMovement`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorMovement {
    #[default]
    After, // 0
    None, // 1
}

impl Display {
    // Cannot fail (the Zig version is `!Display` but never errors).
    fn parse(kv: &Kv) -> Display {
        let mut result = Display::default();

        if let Some(v) = kv.get(b'i') {
            result.image_id = v;
        }

        if let Some(v) = kv.get(b'I') {
            result.image_number = v;
        }

        if let Some(v) = kv.get(b'p') {
            result.placement_id = v;
        }

        if let Some(v) = kv.get(b'x') {
            result.x = v;
        }

        if let Some(v) = kv.get(b'y') {
            result.y = v;
        }

        if let Some(v) = kv.get(b'w') {
            result.width = v;
        }

        if let Some(v) = kv.get(b'h') {
            result.height = v;
        }

        if let Some(v) = kv.get(b'X') {
            result.x_offset = v;
        }

        if let Some(v) = kv.get(b'Y') {
            result.y_offset = v;
        }

        if let Some(v) = kv.get(b'c') {
            result.columns = v;
        }

        if let Some(v) = kv.get(b'r') {
            result.rows = v;
        }

        if let Some(v) = kv.get(b'C') {
            // Kitty only tests this against 1 so any other value moves the cursor.
            result.cursor_movement = if v == 1 {
                CursorMovement::None
            } else {
                CursorMovement::After
            };
        }

        if let Some(v) = kv.get(b'U') {
            // Kitty stores this in a bool, so any non-zero value is virtual.
            result.virtual_placement = v != 0;
        }

        if let Some(v) = kv.get(b'z') {
            // We can bitcast here because of how we parse it earlier.
            result.z = v as i32;
        }

        if let Some(v) = kv.get(b'P') {
            result.parent_id = v;
        }

        if let Some(v) = kv.get(b'Q') {
            result.parent_placement_id = v;
        }

        if let Some(v) = kv.get(b'H') {
            // We can bitcast here because of how we parse it earlier.
            result.horizontal_offset = v as i32;
        }

        if let Some(v) = kv.get(b'V') {
            // We can bitcast here because of how we parse it earlier.
            result.vertical_offset = v as i32;
        }

        result
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AnimationFrameLoading {
    /// Frame data is transmitted exactly like image data, so an a=f
    /// command carries a full set of transmission keys: medium (t),
    /// format (f), the frame rectangle size (s/v), chunking (m), etc.
    /// The image id/number here identify the (existing) image the
    /// frame belongs to.
    pub transmission: Transmission,

    /// The left/top edge (pixels) of the rectangle within the frame
    /// that the transmitted data updates.
    pub x: u32, // x
    pub y: u32, // y

    /// The 1-based frame number whose pixels serve as the base canvas
    /// for a newly created frame. Zero means no base frame: the
    /// canvas is filled with `background` instead.
    pub create_frame: u32, // c

    /// The 1-based frame number of an existing frame to edit. Zero
    /// (or any value past the next frame number) creates a new frame.
    pub edit_frame: u32, // r

    /// The gap in milliseconds before the next frame is shown. For
    /// new frames, zero selects the default gap and a negative value
    /// creates a gapless (never displayed) frame. For edits, zero
    /// leaves the gap unchanged.
    pub gap_ms: i32, // z

    pub composition_mode: CompositionMode, // X
    pub background: Background,            // Y
}

/// The canvas background color as a 32-bit RGBA value where R is
/// the most significant byte (0xRRGGBBAA), e.g. Y=4278190335
/// (0xff0000ff) is opaque red. Bit-cast from the little-endian
/// u32 value, so the field order is reversed from the byte order.
///
/// `packed struct(u32) { a, b, g, r }`: `a` is the least significant byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Background {
    pub a: u8,
    pub b: u8,
    pub g: u8,
    pub r: u8,
}

impl Background {
    /// `@bitCast(u32)`
    pub const fn from_bits(v: u32) -> Background {
        Background {
            a: v as u8,
            b: (v >> 8) as u8,
            g: (v >> 16) as u8,
            r: (v >> 24) as u8,
        }
    }

    /// `@bitCast` back to the `u32` representation.
    pub const fn to_bits(self) -> u32 {
        (self.a as u32) | ((self.b as u32) << 8) | ((self.g as u32) << 16) | ((self.r as u32) << 24)
    }
}

impl AnimationFrameLoading {
    fn parse(kv: &Kv) -> Result<AnimationFrameLoading, ParseError> {
        let mut result = AnimationFrameLoading {
            transmission: Transmission::parse(kv)?,
            ..Default::default()
        };

        if let Some(v) = kv.get(b'x') {
            result.x = v;
        }

        if let Some(v) = kv.get(b'y') {
            result.y = v;
        }

        if let Some(v) = kv.get(b'c') {
            result.create_frame = v;
        }

        if let Some(v) = kv.get(b'r') {
            result.edit_frame = v;
        }

        if let Some(v) = kv.get(b'z') {
            // 'z' is one of the keys Parser.finishValue parses as an
            // i32, so the u32 in the KV table is the bit pattern of
            // that i32 and this reconstructs it exactly.
            result.gap_ms = v as i32;
        }

        // Tested only against 1 like Kitty.
        if let Some(v) = kv.get(b'X') {
            result.composition_mode = if v == 1 {
                CompositionMode::Overwrite
            } else {
                CompositionMode::AlphaBlend
            };
        }

        if let Some(v) = kv.get(b'Y') {
            result.background = Background::from_bits(v);
        }

        Ok(result)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AnimationFrameComposition {
    pub image_id: u32,     // i
    pub image_number: u32, // I
    pub placement_id: u32, // p

    /// The 1-based frame number being edited (the destination).
    pub dest_frame: u32, // c

    /// The 1-based frame number whose pixels are composed onto the
    /// destination (the source).
    pub source_frame: u32, // r

    /// The left/top edge (pixels) of the destination rectangle.
    pub x: u32, // x
    pub y: u32, // y

    /// The size of the rectangle to compose. Zero selects the full
    /// image width/height.
    pub width: u32, // w
    pub height: u32, // h

    /// The left/top edge (pixels) of the source rectangle.
    pub left_edge: u32, // X
    pub top_edge: u32, // Y

    pub composition_mode: CompositionMode, // C
}

impl AnimationFrameComposition {
    // Cannot fail: every key is stored as parsed, with no range
    // validation (Kitty range-checks these while handling the
    // command; see the comment above the execution-deferral tests).
    fn parse(kv: &Kv) -> AnimationFrameComposition {
        let mut result = AnimationFrameComposition::default();

        if let Some(v) = kv.get(b'i') {
            result.image_id = v;
        }

        if let Some(v) = kv.get(b'I') {
            result.image_number = v;
        }

        if let Some(v) = kv.get(b'p') {
            result.placement_id = v;
        }

        if let Some(v) = kv.get(b'c') {
            result.dest_frame = v;
        }

        if let Some(v) = kv.get(b'r') {
            result.source_frame = v;
        }

        if let Some(v) = kv.get(b'x') {
            result.x = v;
        }

        if let Some(v) = kv.get(b'y') {
            result.y = v;
        }

        if let Some(v) = kv.get(b'w') {
            result.width = v;
        }

        if let Some(v) = kv.get(b'h') {
            result.height = v;
        }

        if let Some(v) = kv.get(b'X') {
            result.left_edge = v;
        }

        if let Some(v) = kv.get(b'Y') {
            result.top_edge = v;
        }

        if let Some(v) = kv.get(b'C') {
            // Kitty tests this against zero for compose so any non-zero overwrites
            result.composition_mode = if v == 0 {
                CompositionMode::AlphaBlend
            } else {
                CompositionMode::Overwrite
            };
        }

        result
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AnimationControl {
    pub image_id: u32,           // i
    pub image_number: u32,       // I
    pub placement_id: u32,       // p
    pub action: AnimationAction, // s

    /// The 1-based frame number whose gap is set from gap_ms.
    pub frame: u32, // r

    /// The new gap for `frame` in milliseconds. Zero is ignored and a
    /// negative value makes the frame gapless.
    pub gap_ms: i32, // z

    /// The 1-based frame number to make current (client-driven
    /// animation). Zero is ignored.
    pub current_frame: u32, // c

    /// The loop count: zero is ignored, 1 loops forever, and any
    /// larger value plays that many minus one loops.
    pub loops: u32, // v
}

/// `AnimationControl.AnimationAction`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AnimationAction {
    #[default]
    Invalid, // 0
    Stop,    // 1
    RunWait, // 2
    Run,     // 3
}

impl AnimationControl {
    // Cannot fail: every key is stored as parsed. Out-of-range values
    // (frame numbers, states) are silently ignored during execution,
    // matching Kitty.
    fn parse(kv: &Kv) -> AnimationControl {
        let mut result = AnimationControl::default();

        if let Some(v) = kv.get(b'i') {
            result.image_id = v;
        }

        if let Some(v) = kv.get(b'I') {
            result.image_number = v;
        }

        if let Some(v) = kv.get(b'p') {
            result.placement_id = v;
        }

        if let Some(v) = kv.get(b's') {
            // Kitty ignores values it doesn't know, leaving the animation
            // state untouched, which is what `.invalid` means here.
            result.action = match v {
                1 => AnimationAction::Stop,
                2 => AnimationAction::RunWait,
                3 => AnimationAction::Run,
                _ => AnimationAction::Invalid,
            };
        }

        if let Some(v) = kv.get(b'r') {
            result.frame = v;
        }

        if let Some(v) = kv.get(b'z') {
            // 'z' is one of the keys Parser.finishValue parses as an
            // i32, so the u32 in the KV table is the bit pattern of
            // that i32 and this reconstructs it exactly.
            result.gap_ms = v as i32;
        }

        if let Some(v) = kv.get(b'c') {
            result.current_frame = v;
        }

        if let Some(v) = kv.get(b'v') {
            result.loops = v;
        }

        result
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Delete {
    pub image_id: u32,     // i
    pub image_number: u32, // I
    pub placement_id: u32, // p
    pub action: DeleteAction,
}

impl Delete {
    fn parse(kv: &Kv) -> Result<Delete, ParseError> {
        Ok(Delete {
            image_id: kv.get(b'i').unwrap_or(0),
            image_number: kv.get(b'I').unwrap_or(0),
            placement_id: kv.get(b'p').unwrap_or(0),
            action: DeleteAction::parse(kv)?,
        })
    }
}

/// `Delete.Action`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteAction {
    // a/A
    All(bool),

    // i/I
    Id(DeleteId),

    // n/N
    Newest(DeleteNewest),

    // c/C,
    IntersectCursor(bool),

    // f/F
    AnimationFrames(AnimationFrames),

    // p/P
    IntersectCell(DeleteIntersectCell),

    // q/Q
    IntersectCellZ(DeleteIntersectCellZ),

    // r/R
    Range(DeleteRange),

    // x/X
    Column(DeleteColumn),

    // y/Y
    Row(DeleteRow),

    // z/Z
    Z(DeleteZ),
}

/// Payload of [`DeleteAction::Id`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeleteId {
    pub delete: bool,      // uppercase
    pub image_id: u32,     // i
    pub placement_id: u32, // p
}

/// Payload of [`DeleteAction::Newest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeleteNewest {
    pub delete: bool,      // uppercase
    pub image_number: u32, // I
    pub placement_id: u32, // p
}

/// Payload of [`DeleteAction::IntersectCell`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeleteIntersectCell {
    pub delete: bool, // uppercase
    pub x: u32,       // x
    pub y: u32,       // y
}

/// Payload of [`DeleteAction::IntersectCellZ`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeleteIntersectCellZ {
    pub delete: bool, // uppercase
    pub x: u32,       // x
    pub y: u32,       // y
    pub z: i32,       // z
}

/// Payload of [`DeleteAction::Range`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeleteRange {
    pub delete: bool, // uppercase
    pub first: u32,   // x
    pub last: u32,    // y
}

/// Payload of [`DeleteAction::Column`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeleteColumn {
    pub delete: bool, // uppercase
    pub x: u32,       // x
}

/// Payload of [`DeleteAction::Row`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeleteRow {
    pub delete: bool, // uppercase
    pub y: u32,       // y
}

/// Payload of [`DeleteAction::Z`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeleteZ {
    pub delete: bool, // uppercase
    pub z: i32,       // z
}

/// `Delete.Action.AnimationFrames`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AnimationFrames {
    pub delete: bool,      // uppercase
    pub image_id: u32,     // i
    pub image_number: u32, // I
    pub frame: u32,        // r
}

impl DeleteAction {
    fn parse(kv: &Kv) -> Result<DeleteAction, ParseError> {
        let what: u8 = match kv.get(b'd') {
            None => b'a',
            Some(value) => u8::try_from(value).map_err(|_| ParseError::InvalidFormat)?,
        };

        Ok(match what {
            b'a' | b'A' => DeleteAction::All(what == b'A'),

            b'i' | b'I' => {
                let mut result = DeleteId {
                    delete: what == b'I',
                    ..Default::default()
                };
                if let Some(v) = kv.get(b'i') {
                    result.image_id = v;
                }
                if let Some(v) = kv.get(b'p') {
                    result.placement_id = v;
                }
                DeleteAction::Id(result)
            }

            b'n' | b'N' => {
                let mut result = DeleteNewest {
                    delete: what == b'N',
                    ..Default::default()
                };
                if let Some(v) = kv.get(b'I') {
                    result.image_number = v;
                }
                if let Some(v) = kv.get(b'p') {
                    result.placement_id = v;
                }
                DeleteAction::Newest(result)
            }

            b'c' | b'C' => DeleteAction::IntersectCursor(what == b'C'),

            b'f' | b'F' => {
                let mut result = AnimationFrames {
                    delete: what == b'F',
                    ..Default::default()
                };
                if let Some(v) = kv.get(b'i') {
                    result.image_id = v;
                }
                if let Some(v) = kv.get(b'I') {
                    result.image_number = v;
                }
                if let Some(v) = kv.get(b'r') {
                    result.frame = v;
                }
                DeleteAction::AnimationFrames(result)
            }

            b'p' | b'P' => {
                let mut result = DeleteIntersectCell {
                    delete: what == b'P',
                    ..Default::default()
                };
                if let Some(v) = kv.get(b'x') {
                    result.x = v;
                }
                if let Some(v) = kv.get(b'y') {
                    result.y = v;
                }
                DeleteAction::IntersectCell(result)
            }

            b'q' | b'Q' => {
                let mut result = DeleteIntersectCellZ {
                    delete: what == b'Q',
                    ..Default::default()
                };
                if let Some(v) = kv.get(b'x') {
                    result.x = v;
                }
                if let Some(v) = kv.get(b'y') {
                    result.y = v;
                }
                if let Some(v) = kv.get(b'z') {
                    // We can bitcast here because of how we parse it earlier.
                    result.z = v as i32;
                }
                DeleteAction::IntersectCellZ(result)
            }

            b'r' | b'R' => {
                // Both bounds default to zero when omitted and no
                // validation is performed; an inverted or zero range
                // simply matches no images.
                let mut result = DeleteRange {
                    delete: what == b'R',
                    ..Default::default()
                };
                if let Some(v) = kv.get(b'x') {
                    result.first = v;
                }
                if let Some(v) = kv.get(b'y') {
                    result.last = v;
                }
                DeleteAction::Range(result)
            }

            b'x' | b'X' => {
                let mut result = DeleteColumn {
                    delete: what == b'X',
                    ..Default::default()
                };
                if let Some(v) = kv.get(b'x') {
                    result.x = v;
                }
                DeleteAction::Column(result)
            }

            b'y' | b'Y' => {
                let mut result = DeleteRow {
                    delete: what == b'Y',
                    ..Default::default()
                };
                if let Some(v) = kv.get(b'y') {
                    result.y = v;
                }
                DeleteAction::Row(result)
            }

            b'z' | b'Z' => {
                let mut result = DeleteZ {
                    delete: what == b'Z',
                    ..Default::default()
                };
                if let Some(v) = kv.get(b'z') {
                    // We can bitcast here because of how we parse it earlier.
                    result.z = v as i32;
                }
                DeleteAction::Z(result)
            }

            _ => return Err(ParseError::InvalidFormat),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompositionMode {
    #[default]
    AlphaBlend, // 0
    Overwrite, // 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &str) -> Result<Command, ParseError> {
        let mut p = Parser::init(1024 * 1024);
        for &c in input.as_bytes() {
            p.feed(c)?;
        }
        p.complete()
    }

    #[test]
    fn transmission_command() {
        let command = parse("f=24,s=10,v=20").unwrap();

        let Control::Transmit(v) = command.control else {
            panic!()
        };
        assert_eq!(Format::Rgb, v.format);
        assert_eq!(10u32, v.width);
        assert_eq!(20u32, v.height);
        assert!(!v.usage.transient);
    }

    #[test]
    fn transmission_command_with_transient_hint() {
        let command = parse("f=24,s=10,v=20,N=1").unwrap();

        let Control::Transmit(v) = command.control else {
            panic!()
        };
        assert_eq!(Format::Rgb, v.format);
        assert_eq!(10u32, v.width);
        assert_eq!(20u32, v.height);
        assert!(v.usage.transient);
    }

    #[test]
    fn feed_slice_matches_per_byte_feed() {
        let input = b"f=24,s=10,v=20;aGVsbG8gd29ybGQ=";

        let mut p1 = Parser::init(1024 * 1024);
        for &c in input {
            p1.feed(c).unwrap();
        }
        let c1 = p1.complete().unwrap();

        let mut p2 = Parser::init(1024 * 1024);
        p2.feed_slice(input).unwrap();
        let c2 = p2.complete().unwrap();

        assert!(matches!(c1.control, Control::Transmit(_)));
        assert!(matches!(c2.control, Control::Transmit(_)));
        assert_eq!(c1.data, c2.data);
    }

    #[test]
    fn feed_slice_across_slice_boundaries() {
        let mut p = Parser::init(1024 * 1024);

        p.feed_slice(b"f=24,s=10").unwrap();
        p.feed_slice(b",v=20;aGVsbG8g").unwrap();
        p.feed_slice(b"d29ybGQ=").unwrap();
        let command = p.complete().unwrap();

        assert!(matches!(command.control, Control::Transmit(_)));
        // The payload is base64-decoded on completion.
        assert_eq!(b"hello world", &command.data[..]);
    }

    #[test]
    fn feed_slice_respects_max_bytes() {
        let mut p = Parser::init(4);

        p.feed_slice(b"f=24;ab").unwrap();
        assert_eq!(Err(ParseError::OutOfMemory), p.feed_slice(b"cde"));
    }

    #[test]
    fn transmission_ignores_m_if_medium_is_not_direct() {
        let command = parse("a=t,t=t,m=1").unwrap();

        let Control::Transmit(v) = command.control else {
            panic!()
        };
        assert_eq!(Medium::TemporaryFile, v.medium);
        assert!(!v.more_chunks);
    }

    #[test]
    fn transmission_respects_m_if_medium_is_direct() {
        let command = parse("a=t,t=d,m=1").unwrap();

        let Control::Transmit(v) = command.control else {
            panic!()
        };
        assert_eq!(Medium::Direct, v.medium);
        assert!(v.more_chunks);
    }

    #[test]
    fn query_command() {
        let command = parse("i=31,s=1,v=1,a=q,t=d,f=24;QUFBQQ").unwrap();

        let Control::Query(v) = command.control else {
            panic!()
        };
        assert_eq!(Medium::Direct, v.medium);
        assert_eq!(1u32, v.width);
        assert_eq!(1u32, v.height);
        assert_eq!(31u32, v.image_id);
        assert_eq!(b"AAAA", &command.data[..]);
    }

    #[test]
    fn display_command() {
        let command = parse("a=p,U=1,i=31,c=80,r=120").unwrap();

        let Control::Display(v) = command.control else {
            panic!()
        };
        assert_eq!(80u32, v.columns);
        assert_eq!(120u32, v.rows);
        assert_eq!(31u32, v.image_id);
    }

    #[test]
    fn delete_command() {
        let command = parse("a=d,d=p,x=3,y=4").unwrap();

        let Control::Delete(d) = command.control else {
            panic!()
        };
        let DeleteAction::IntersectCell(dv) = d.action else {
            panic!()
        };
        assert!(!dv.delete);
        assert_eq!(3u32, dv.x);
        assert_eq!(4u32, dv.y);
    }

    #[test]
    fn no_control_data() {
        let command = parse(";QUFBQQ").unwrap();

        assert!(matches!(command.control, Control::Transmit(_)));
        assert_eq!(b"AAAA", &command.data[..]);
    }

    #[test]
    fn ignore_unknown_keys_long() {
        let command = parse("f=24,s=10,v=20,hello=world").unwrap();

        let Control::Transmit(v) = command.control else {
            panic!()
        };
        assert_eq!(Format::Rgb, v.format);
        assert_eq!(10u32, v.width);
        assert_eq!(20u32, v.height);
    }

    #[test]
    fn ignore_unknown_keys_non_letter() {
        let command = parse("f=24,s=10,v=20,!=1").unwrap();

        let Control::Transmit(v) = command.control else {
            panic!()
        };
        assert_eq!(Format::Rgb, v.format);
        assert_eq!(10u32, v.width);
        assert_eq!(20u32, v.height);
    }

    #[test]
    fn ignore_very_long_values() {
        let command = parse("f=24,s=10,v=2000000000000000000000000000000000000000").unwrap();

        let Control::Transmit(v) = command.control else {
            panic!()
        };
        assert_eq!(Format::Rgb, v.format);
        assert_eq!(10u32, v.width);
        assert_eq!(0u32, v.height);
    }

    #[test]
    fn ensure_very_large_negative_values_dont_get_skipped() {
        let command = parse("a=p,i=1,z=-2000000000").unwrap();

        let Control::Display(v) = command.control else {
            panic!()
        };
        assert_eq!(1, v.image_id);
        assert_eq!(-2000000000, v.z);
    }

    #[test]
    fn ensure_proper_overflow_error_for_u32() {
        assert_eq!(Err(ParseError::Overflow), parse("a=p,i=10000000000"));
    }

    #[test]
    fn ensure_proper_overflow_error_for_i32() {
        assert_eq!(Err(ParseError::Overflow), parse("a=p,i=1,z=-9999999999"));
    }

    #[test]
    fn all_i32_values() {
        {
            // 'z' (usually z-axis values)
            let command = parse("a=p,i=1,z=-1").unwrap();
            let Control::Display(v) = command.control else {
                panic!()
            };
            assert_eq!(1, v.image_id);
            assert_eq!(-1, v.z);
        }

        {
            // 'H' (relative placement, horizontal offset)
            let command = parse("a=p,i=1,H=-1").unwrap();
            let Control::Display(v) = command.control else {
                panic!()
            };
            assert_eq!(1, v.image_id);
            assert_eq!(-1, v.horizontal_offset);
        }

        {
            // 'V' (relative placement, vertical offset)
            let command = parse("a=p,i=1,V=-1").unwrap();
            let Control::Display(v) = command.control else {
                panic!()
            };
            assert_eq!(1, v.image_id);
            assert_eq!(-1, v.vertical_offset);
        }
    }

    fn encode(r: &Response) -> Vec<u8> {
        let mut buf = Vec::new();
        r.encode(&mut buf).unwrap();
        buf
    }

    #[test]
    fn response_encode_nothing_without_id_or_image_number() {
        let r = Response::default();
        assert_eq!(b"", &encode(&r)[..]);
    }

    #[test]
    fn response_encode_with_only_image_id() {
        let r = Response {
            id: 4,
            ..Default::default()
        };
        assert_eq!(b"\x1b_Gi=4;OK\x1b\\", &encode(&r)[..]);
    }

    #[test]
    fn response_encode_with_only_image_number() {
        let r = Response {
            image_number: 4,
            ..Default::default()
        };
        assert_eq!(b"\x1b_GI=4;OK\x1b\\", &encode(&r)[..]);
    }

    #[test]
    fn response_encode_with_image_id_and_number() {
        let r = Response {
            id: 12,
            image_number: 4,
            ..Default::default()
        };
        assert_eq!(b"\x1b_Gi=12,I=4;OK\x1b\\", &encode(&r)[..]);
    }

    fn delete_range(input: &str) -> DeleteRange {
        let command = parse(input).unwrap();
        let Control::Delete(d) = command.control else {
            panic!()
        };
        let DeleteAction::Range(range) = d.action else {
            panic!()
        };
        range
    }

    #[test]
    fn delete_range_command_1() {
        let range = delete_range("a=d,d=r,x=3,y=4");
        assert!(!range.delete);
        assert_eq!(3u32, range.first);
        assert_eq!(4u32, range.last);
    }

    #[test]
    fn delete_range_command_2() {
        let range = delete_range("a=d,d=R,x=5,y=11");
        assert!(range.delete);
        assert_eq!(5u32, range.first);
        assert_eq!(11u32, range.last);
    }

    #[test]
    fn delete_range_command_3() {
        // An inverted range is accepted; it simply matches no images. This
        // matches kitty, which does no validation and filters with
        // `x <= image_id <= y`.
        let range = delete_range("a=d,d=R,x=5,y=4");
        assert!(range.delete);
        assert_eq!(5u32, range.first);
        assert_eq!(4u32, range.last);
    }

    #[test]
    fn delete_range_command_4() {
        // An omitted upper bound defaults to zero, which matches no images
        // since image IDs are always non-zero.
        let range = delete_range("a=d,d=R,x=5");
        assert!(range.delete);
        assert_eq!(5u32, range.first);
        assert_eq!(0u32, range.last);
    }

    #[test]
    fn delete_range_command_5() {
        let range = delete_range("a=d,d=R,y=5");
        assert!(range.delete);
        assert_eq!(0u32, range.first);
        assert_eq!(5u32, range.last);
    }

    #[test]
    fn delete_range_command_6() {
        // Both bounds omitted defaults to the empty range [0, 0].
        let range = delete_range("a=d,d=r");
        assert!(!range.delete);
        assert_eq!(0u32, range.first);
        assert_eq!(0u32, range.last);
    }

    // Kitty range-checks non-flag keys while handling the command, not while
    // parsing, so a bad value never drops the command (which would make a
    // response impossible even when the client gave an id). The flag keys
    // "a", "d", "t" and "o" are the exception.

    #[test]
    fn unknown_format_value_is_deferred_to_execution() {
        let command = Parser::parse_string(b"a=t,f=42,i=31,s=1,v=1").unwrap();

        let Control::Transmit(v) = command.control else {
            panic!()
        };
        assert!(v.format_unknown);
        assert_eq!(31u32, v.image_id);
    }

    #[test]
    fn zero_format_value_is_rgba() {
        let command = Parser::parse_string(b"a=t,f=0,i=31,s=1,v=1").unwrap();

        let Control::Transmit(v) = command.control else {
            panic!()
        };
        assert!(!v.format_unknown);
        assert_eq!(Format::Rgba, v.format);
    }

    #[test]
    fn known_format_values_are_not_unknown() {
        for (input, format) in [
            ("f=24", Format::Rgb),
            ("f=32", Format::Rgba),
            ("f=100", Format::Png),
        ] {
            let command = Parser::parse_string(input.as_bytes()).unwrap();

            let Control::Transmit(v) = command.control else {
                panic!()
            };
            assert!(!v.format_unknown);
            assert_eq!(format, v.format);
        }
    }

    #[test]
    fn quiet_value_above_two_suppresses_all_responses() {
        let command = Parser::parse_string(b"a=t,q=3,i=31,s=1,v=1").unwrap();
        assert_eq!(Quiet::Failures, command.quiet);
    }

    #[test]
    fn cursor_movement_value_above_one_moves_the_cursor() {
        let command = Parser::parse_string(b"a=p,i=31,C=2").unwrap();

        let Control::Display(d) = command.control else {
            panic!()
        };
        assert_eq!(CursorMovement::After, d.cursor_movement);
    }

    #[test]
    fn virtual_placement_value_above_one_is_virtual() {
        let command = Parser::parse_string(b"a=p,i=31,U=2").unwrap();

        let Control::Display(d) = command.control else {
            panic!()
        };
        assert!(d.virtual_placement);
    }

    #[test]
    fn animation_frame_composition_value_above_one_alpha_blends() {
        let command = Parser::parse_string(b"a=f,i=31,X=2").unwrap();

        let Control::TransmitAnimationFrame(f) = command.control else {
            panic!()
        };
        assert_eq!(CompositionMode::AlphaBlend, f.composition_mode);
    }

    #[test]
    fn animation_compose_value_above_one_overwrites() {
        let command = Parser::parse_string(b"a=c,i=31,C=2").unwrap();

        let Control::ComposeAnimation(c) = command.control else {
            panic!()
        };
        assert_eq!(CompositionMode::Overwrite, c.composition_mode);
    }

    #[test]
    fn animation_control_action_above_three_is_ignored() {
        let command = Parser::parse_string(b"a=a,i=31,s=4").unwrap();

        let Control::ControlAnimation(a) = command.control else {
            panic!()
        };
        assert_eq!(AnimationAction::Invalid, a.action);
    }

    #[test]
    fn unknown_flag_key_values_still_fail_parsing() {
        // Kitty rejects these while parsing too. See gen/apc_parsers.py.
        for input in [
            "a=z,i=31",     // action
            "a=d,d=w,i=31", // delete action
            "a=t,t=q,i=31", // transmission medium
            "a=t,o=q,i=31", // compression
        ] {
            assert_eq!(
                Err(ParseError::InvalidFormat),
                Parser::parse_string(input.as_bytes())
            );
        }
    }

    // Not in the Zig file: exercise the packed-struct bitcasts.
    #[test]
    fn packed_bitcasts_roundtrip() {
        let bg = Background::from_bits(0xff0000ff);
        assert_eq!(
            Background {
                r: 0xff,
                g: 0,
                b: 0,
                a: 0xff
            },
            bg
        );
        assert_eq!(0xff0000ff, bg.to_bits());
        let usage = Usage::from_bits(0b101);
        assert!(usage.transient);
        assert_eq!(0b101, usage.to_bits());
    }
}
