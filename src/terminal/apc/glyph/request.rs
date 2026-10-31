//! Port of `src/terminal/apc/glyph/request.zig`.
//!
//! STATUS: ported
//!
//! Port notes / deviations:
//! - Allocator parameters are dropped; requests own their raw bytes as a
//!   `Vec<u8>` and free them on drop (Zig: `Request.deinit`).
//! - The comptime option enums (`Query.Option`, `Register.Option`,
//!   `Clear.Option`) whose `Type()` depends on the tag are expressed as
//!   zero-sized marker types in [`query_option`], [`register_option`] and
//!   [`clear_option`] implementing [`RequestOption`]; `get(.cp)` becomes
//!   `get(register_option::Cp)`.
//! - `std.fmt.parseInt` and `std.base64.standard.Decoder` semantics are
//!   reproduced by local helpers.
//! - `decodeGlyfPayload` passes a `LimitedAllocator` to the glyf decoder in
//!   Zig. The Rust glyf decoder allocates with `Vec`, so the limit is
//!   enforced by [`check_glyf_allocations`], which replays the decoder's
//!   allocation sequence (end points, points, then flags beyond the
//!   4096-byte stack buffer) against a [`LimitedAllocator`] before decoding.

use std::fmt;

use crate::datastruct::limited_allocator::LimitedAllocator;
use crate::font::opentype::glyf::{self, Outline};
use crate::terminal::fraction;

/// Maximum decoded glyph payload size accepted by the protocol.
/// This is documented in the spec.
const MAX_PAYLOAD_SIZE: usize = 64 * 1024; // 64 KiB

/// `CommandParser.Error`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    OutOfMemory,
    InvalidFormat,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for Error {}

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

impl From<AllocError> for Error {
    fn from(_: AllocError) -> Self {
        Error::OutOfMemory
    }
}

/// `error{InvalidFormat}` returned by [`Request::parse`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    InvalidFormat,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for ParseError {}

impl From<ParseError> for Error {
    fn from(_: ParseError) -> Self {
        Error::InvalidFormat
    }
}

/// Stateful parser for a single glyph APC payload after the `25a1;` prefix.
#[derive(Debug, Clone)]
pub struct CommandParser {
    pub data: Vec<u8>,

    /// Maximum bytes the data payload can buffer. This is to prevent
    /// malicious input from causing us to allocate too much memory.
    pub max_bytes: usize,
}

impl CommandParser {
    /// Create a glyph APC parser that buffers the raw command bytes.
    pub fn init(max_bytes: usize) -> CommandParser {
        CommandParser {
            data: Vec::new(),
            max_bytes,
        }
    }

    /// Append one more byte of APC payload to the buffered command.
    pub fn feed(&mut self, byte: u8) -> Result<(), AllocError> {
        if self.data.len() >= self.max_bytes {
            return Err(AllocError::OutOfMemory);
        }
        self.data
            .try_reserve(1)
            .map_err(|_| AllocError::OutOfMemory)?;
        self.data.push(byte);
        Ok(())
    }

    /// Append a slice of APC payload bytes to the buffered command.
    /// Equivalent to calling feed for each byte, but appends in bulk.
    pub fn feed_slice(&mut self, bytes: &[u8]) -> Result<(), AllocError> {
        if self.data.len() + bytes.len() > self.max_bytes {
            return Err(AllocError::OutOfMemory);
        }
        self.data
            .try_reserve(bytes.len())
            .map_err(|_| AllocError::OutOfMemory)?;
        self.data.extend_from_slice(bytes);
        Ok(())
    }

    /// Finish parsing and return an owned request that can outlive the parser.
    pub fn complete(&mut self) -> Result<Request, Error> {
        // Normalize bare single-byte verbs like `s` into `s;` so the parsed
        // command always has the standard `verb;...` layout.
        if self.data.len() == 1 {
            self.data
                .try_reserve(1)
                .map_err(|_| Error::OutOfMemory)?;
            self.data.push(b';');
        }

        // Ownership of the buffered bytes has moved to `raw`, so clear the
        // array list before we build the final command value.
        let raw = std::mem::take(&mut self.data);
        Ok(Request::parse(raw)?)
    }
}

/// Parsed glyph APC request with the verb classified eagerly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Support query (bare `s` verb, no options).
    Support,

    /// Codepoint coverage query.
    Query(Query),

    /// Glyph registration request.
    Register(Register),

    /// Registration clear request.
    Clear(Clear),
}

/// A lazily-decoded request option (the Zig per-verb `Option` enums).
pub trait RequestOption: Copy {
    /// The decoded type of the option (Zig: `Option.Type()`).
    type Type;

    /// The wire-format option key (Zig: `Option.key()`, the tag name).
    const KEY: &'static [u8];

    /// Decode the option value.
    fn decode(value: &[u8]) -> Option<Self::Type>;

    /// Read and decode the option from the raw option string.
    fn read(self, raw: &[u8]) -> Option<Self::Type> {
        let value = option_value(raw, Self::KEY)?;
        Self::decode(value)
    }
}

/// Options recognized for the glyph query request (`Request.Query.Option`).
pub mod query_option {
    use super::{parse_int, RequestOption};

    /// Target Unicode codepoint encoded in hexadecimal.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Cp;

    impl RequestOption for Cp {
        type Type = u32;
        const KEY: &'static [u8] = b"cp";
        fn decode(value: &[u8]) -> Option<u32> {
            parse_int(value, 16, U21_MAX).map(|v| v as u32)
        }
    }

    pub(super) const U21_MAX: u64 = 0x1F_FFFF;
}

/// Marker for the options of [`Query`].
pub trait QueryOption: RequestOption {}
impl QueryOption for query_option::Cp {}

/// Query verb payload with lazily-decoded options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub raw: Vec<u8>,
}

impl Query {
    /// Initialize a query command from owned raw command bytes.
    pub fn init(raw: Vec<u8>) -> Query {
        Query { raw }
    }

    /// Lazily decode a query option on demand.
    pub fn get<O: QueryOption>(&self, option: O) -> Option<O::Type> {
        option.read(self.raw_options())
    }

    /// Return the raw option portion of a valid query command.
    fn raw_options(&self) -> &[u8] {
        debug_assert!(self.raw.len() >= 2);
        debug_assert!(self.raw[0] == b'q');
        debug_assert!(self.raw[1] == b';');
        &self.raw[2..]
    }
}

/// An option of the glyph register verb (`Request.Register.Option`).
pub trait RegisterOption: RequestOption {
    /// Return the protocol default value for this option, if any.
    fn default(self) -> Option<Self::Type>;

    /// The value used by [`Register::get`] when the option is omitted.
    /// This is the protocol default except for `aw` and `lh`, which
    /// default to `upm`.
    fn omitted(self, _reg: &Register) -> Option<Self::Type> {
        self.default()
    }
}

/// Options recognized for the glyph register verb
/// (`Request.Register.Option`).
pub mod register_option {
    use super::query_option::U21_MAX;
    use super::{parse_int, Register, RegisterOption, RequestOption};

    macro_rules! option {
        ($(#[$doc:meta])* $name:ident, $key:literal, $ty:ty,
         decode = |$v:ident| $decode:expr) => {
            $(#[$doc])*
            #[derive(Debug, Clone, Copy, PartialEq, Eq)]
            pub struct $name;

            impl RequestOption for $name {
                type Type = $ty;
                const KEY: &'static [u8] = $key;
                fn decode($v: &[u8]) -> Option<$ty> {
                    $decode
                }
            }
        };
    }

    option!(
        /// Target Unicode codepoint encoded in hexadecimal.
        Cp, b"cp", u32,
        decode = |value| parse_int(value, 16, U21_MAX).map(|v| v as u32)
    );
    option!(
        /// Glyph payload format.
        Fmt, b"fmt", super::Format,
        decode = |value| super::Format::init(value)
    );
    option!(
        /// Requested reply verbosity for registration.
        Reply, b"reply", super::Reply,
        decode = |value| Some(super::Reply::init(value).unwrap_or(super::Reply::All))
    );
    option!(
        /// Units-per-em for the glyph coordinate system.
        Upm, b"upm", u32,
        decode = |value| parse_int(value, 10, u32::MAX as u64).map(|v| v as u32)
    );
    option!(
        /// Authored advance width in units-per-em units.
        Aw, b"aw", u32,
        decode = |value| parse_int(value, 10, u32::MAX as u64).map(|v| v as u32)
    );
    option!(
        /// Authored line height in units-per-em units.
        Lh, b"lh", u32,
        decode = |value| parse_int(value, 10, u32::MAX as u64).map(|v| v as u32)
    );
    option!(
        /// Unicode cell width for terminal layout.
        Width, b"width", super::Width,
        decode = |value| super::Width::init(value)
    );
    option!(
        /// Glyph scale policy.
        Size, b"size", super::Size,
        decode = |value| super::Size::init(value)
    );
    option!(
        /// Glyph placement within the render span.
        Align, b"align", super::Align,
        decode = |value| super::Align::init(value)
    );
    option!(
        /// Fractional insets from the render span edges.
        Pad, b"pad", super::Pad,
        decode = |value| super::Pad::init(value)
    );

    impl RegisterOption for Cp {
        fn default(self) -> Option<u32> {
            None
        }
    }
    impl RegisterOption for Fmt {
        fn default(self) -> Option<super::Format> {
            Some(super::Format::Glyf)
        }
    }
    impl RegisterOption for Reply {
        fn default(self) -> Option<super::Reply> {
            Some(super::Reply::All)
        }
    }
    impl RegisterOption for Upm {
        fn default(self) -> Option<u32> {
            Some(1000)
        }
    }
    impl RegisterOption for Aw {
        fn default(self) -> Option<u32> {
            None
        }
        fn omitted(self, reg: &Register) -> Option<u32> {
            reg.get(Upm)
        }
    }
    impl RegisterOption for Lh {
        fn default(self) -> Option<u32> {
            None
        }
        fn omitted(self, reg: &Register) -> Option<u32> {
            reg.get(Upm)
        }
    }
    impl RegisterOption for Width {
        fn default(self) -> Option<super::Width> {
            Some(super::Width::Narrow)
        }
    }
    impl RegisterOption for Size {
        fn default(self) -> Option<super::Size> {
            Some(super::Size::Height)
        }
    }
    impl RegisterOption for Align {
        fn default(self) -> Option<super::Align> {
            Some(super::Align::default())
        }
    }
    impl RegisterOption for Pad {
        fn default(self) -> Option<super::Pad> {
            Some(super::Pad::default())
        }
    }
}

/// Errors that can occur while decoding a register glyph payload
/// (`Register.DecodeError`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    OutOfMemory,

    /// The decoded payload exceeds the protocol limit.
    PayloadTooLarge,

    /// The payload could not be decoded or parsed as the declared format.
    MalformedPayload,

    /// The glyf payload is composite, which the protocol forbids.
    CompositeUnsupported,

    /// The glyf payload contains hinting instructions, which the
    /// protocol forbids.
    HintingUnsupported,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for DecodeError {}

/// Register verb payload with lazily-decoded options and optional base64 data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Register {
    pub raw: Vec<u8>,
    pub payload_idx: usize,
}

impl Register {
    /// Initialize a register command from owned raw command bytes.
    pub fn init(raw: Vec<u8>) -> Option<Register> {
        if raw.len() < 2 {
            return None;
        }
        if raw[0] != b'r' {
            return None;
        }
        if raw[1] != b';' {
            return None;
        }
        let payload_idx = memchr::memrchr(b';', &raw)?;
        if payload_idx <= 1 {
            return None;
        }

        Some(Register { raw, payload_idx })
    }

    /// Lazily decode a register option on demand, applying protocol
    /// defaults when the option is omitted.
    pub fn get<O: RegisterOption>(&self, option: O) -> Option<O::Type> {
        let raw = self.raw_options();
        if option_value(raw, O::KEY).is_none() {
            return option.omitted(self);
        }
        option.read(raw)
    }

    /// Return the base64 payload carried by a register request.
    ///
    /// If no payload is present, this returns an empty slice. The returned
    /// bytes may still be invalid base64; this function only exposes the raw
    /// payload segment and does not validate or decode it.
    pub fn payload(&self) -> &[u8] {
        debug_assert!(self.raw.len() >= 2);
        debug_assert!(self.raw[0] == b'r');
        debug_assert!(self.raw[1] == b';');
        if self.payload_idx == self.raw.len() {
            b""
        } else {
            &self.raw[self.payload_idx + 1..]
        }
    }

    /// Decode this request's base64 glyf payload into an owned outline.
    pub fn decode_glyf_payload(&self, max_allocation_bytes: usize) -> Result<Outline, DecodeError> {
        // Prep base64 decoding, initial validation.
        let payload_bytes = self.payload();
        let size =
            base64_calc_size_for_slice(payload_bytes).map_err(|_| DecodeError::MalformedPayload)?;
        if size > MAX_PAYLOAD_SIZE {
            return Err(DecodeError::PayloadTooLarge);
        }

        // Zig decodes into a stack buffer first (falling back to the heap);
        // a Vec bounded by MAX_PAYLOAD_SIZE is equivalent here.
        let mut data = vec![0u8; size];

        // Base64 decode
        base64_decode(&mut data, payload_bytes).map_err(|_| DecodeError::MalformedPayload)?;

        // Glyf.Entry borrows from `data`, but only for the duration of the
        // decode call below. Glyf.Entry.decode returns an owned Outline, so
        // it is safe to free `data` before returning that outline.
        let glyf_entry = glyf::Entry::init(&data).map_err(|_| DecodeError::MalformedPayload)?;

        // The points slice is the dominant decoded allocation. Limiting
        // every allocation made by the decoder prevents a compact record
        // from expanding into an unbounded persistent outline.
        let limited = LimitedAllocator::new(max_allocation_bytes);
        let result = match check_glyf_allocations(&glyf_entry, &limited) {
            Ok(()) => glyf_entry.decode().map_err(CheckError::Decode),
            Err(err) => Err(err),
        };
        result.map_err(|err| match err {
            CheckError::OutOfMemory => {
                if limited.limit_exceeded.get() {
                    DecodeError::PayloadTooLarge
                } else {
                    DecodeError::OutOfMemory
                }
            }
            // Unsupported fields
            CheckError::Decode(glyf::DecodeError::CompositeNotSupported) => {
                DecodeError::CompositeUnsupported
            }
            CheckError::Decode(glyf::DecodeError::InstructionsNotSupported) => {
                DecodeError::HintingUnsupported
            }
            // Various semantic issues
            CheckError::Decode(
                glyf::DecodeError::EndOfStream
                | glyf::DecodeError::EndPointsOutOfOrder
                | glyf::DecodeError::TooManyPoints
                | glyf::DecodeError::CoordinateOverflow,
            ) => DecodeError::MalformedPayload,
        })
    }

    /// Return the raw option portion of a valid register command.
    fn raw_options(&self) -> &[u8] {
        debug_assert!(self.raw.len() >= 2);
        debug_assert!(self.raw[0] == b'r');
        debug_assert!(self.raw[1] == b';');
        debug_assert!(self.payload_idx >= 2);
        debug_assert!(self.payload_idx <= self.raw.len());
        &self.raw[2..self.payload_idx]
    }
}

/// Error of the glyf decode as seen through the limited allocator: the
/// decoder's own errors plus `error.OutOfMemory`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheckError {
    OutOfMemory,
    Decode(glyf::DecodeError),
}

/// Replay the allocations `Glyf.Entry.decode` makes (in order, interleaved
/// with the validation that precedes each one) against `limited`.
///
/// Returns `Ok(())` if every allocation fits (the real decode then runs),
/// or the error the Zig decoder would hit first. An allocation rejected by
/// the limit sets `limited.limit_exceeded` and yields `OutOfMemory`, as
/// the Zig `LimitedAllocator` does.
fn check_glyf_allocations(
    entry: &glyf::Entry<'_>,
    limited: &LimitedAllocator,
) -> Result<(), CheckError> {
    use glyf::DecodeError as E;

    /// `BufferFirstAllocator` stack buffer size used for flags.
    const FLAGS_STACK_BYTES: usize = 4096;

    // Composite glyphs fail before any allocation.
    if entry.header.number_of_contours < 0 {
        return Ok(());
    }
    let num_contours = entry.header.number_of_contours as usize;
    if num_contours == 0 && entry.data.len() < 2 {
        return Ok(());
    }

    // `alloc.alloc(sfnt.uint16, num_contours)`
    alloc_check(limited, num_contours * std::mem::size_of::<u16>())?;
    if num_contours == 0 {
        return Ok(());
    }

    let data = entry.data;
    let read_u16 = |off: usize| -> Result<u16, CheckError> {
        if off + 2 > data.len() {
            return Err(CheckError::Decode(E::EndOfStream));
        }
        Ok(u16::from_be_bytes([data[off], data[off + 1]]))
    };

    let mut prev_end_point: isize = -1;
    let mut last: u16 = 0;
    for i in 0..num_contours {
        let index = read_u16(i * 2)?;
        if index as isize <= prev_end_point {
            return Err(CheckError::Decode(E::EndPointsOutOfOrder));
        }
        prev_end_point = index as isize;
        last = index;
    }
    let point_count = last as usize + 1;

    let instructions_length = read_u16(num_contours * 2)?;
    if instructions_length > 0 {
        return Err(CheckError::Decode(E::InstructionsNotSupported));
    }

    // `alloc.alloc(Glyf.Outline.Point, point_count)`
    alloc_check(limited, point_count * std::mem::size_of::<glyf::Point>())?;

    // Flags (`SimpleFlags` is a packed u8): stack buffer first, then the
    // limited allocator.
    if point_count > FLAGS_STACK_BYTES {
        alloc_check(limited, point_count)?;
    }

    Ok(())
}

/// The size check `LimitedAllocator` performs for an allocation of `bytes`.
fn alloc_check(limited: &LimitedAllocator, bytes: usize) -> Result<(), CheckError> {
    if bytes <= limited.limit {
        return Ok(());
    }
    limited.limit_exceeded.set(true);
    Err(CheckError::OutOfMemory)
}

/// Options recognized for the glyph clear request (`Request.Clear.Option`).
pub mod clear_option {
    use super::query_option::U21_MAX;
    use super::{parse_int, RequestOption};

    /// Target Unicode codepoint encoded in hexadecimal.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Cp;

    impl RequestOption for Cp {
        type Type = u32;
        const KEY: &'static [u8] = b"cp";
        fn decode(value: &[u8]) -> Option<u32> {
            parse_int(value, 16, U21_MAX).map(|v| v as u32)
        }
    }
}

/// Marker for the options of [`Clear`].
pub trait ClearOption: RequestOption {
    /// Return whether the option is present in the raw option string,
    /// independent of whether its value can be decoded.
    fn present(self, raw: &[u8]) -> bool {
        option_value(raw, Self::KEY).is_some()
    }
}
impl ClearOption for clear_option::Cp {}

/// Clear verb payload with lazily-decoded options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clear {
    pub raw: Vec<u8>,
}

impl Clear {
    /// Initialize a clear command from owned raw command bytes.
    pub fn init(raw: Vec<u8>) -> Clear {
        Clear { raw }
    }

    /// Lazily decode a clear option on demand.
    pub fn get<O: ClearOption>(&self, option: O) -> Option<O::Type> {
        option.read(self.raw_options())
    }

    /// Return whether a clear option was provided, even if malformed.
    pub fn has<O: ClearOption>(&self, option: O) -> bool {
        option.present(self.raw_options())
    }

    /// Return the raw option portion of a valid clear command.
    fn raw_options(&self) -> &[u8] {
        debug_assert!(self.raw.len() >= 2);
        debug_assert!(self.raw[0] == b'c');
        debug_assert!(self.raw[1] == b';');
        &self.raw[2..]
    }
}

impl Request {
    /// Parse an owned glyph APC payload into its eagerly-classified request
    /// form.
    ///
    /// The raw format here is strict on its requirements to avoid
    /// edge cases: it must contain the request AND the request must
    /// end in a semicolon (even if there are no options). The spec itself
    /// does not require this but we artificially insert it in our parser
    /// to simplify parsing later.
    pub fn parse(raw: Vec<u8>) -> Result<Request, ParseError> {
        if raw.len() < 2 {
            return Err(ParseError::InvalidFormat);
        }
        if raw[1] != b';' {
            return Err(ParseError::InvalidFormat);
        }

        match raw[0] {
            b's' => Ok(Request::Support),
            b'q' => Ok(Request::Query(Query::init(raw))),
            b'r' => Ok(Request::Register(
                Register::init(raw).ok_or(ParseError::InvalidFormat)?,
            )),
            b'c' => Ok(Request::Clear(Clear::init(raw))),
            _ => Err(ParseError::InvalidFormat),
        }
    }
}

/// Glyph payload formats named by the protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// TrueType simple glyph outline data.
    Glyf,

    /// OpenType COLR version 0 layered color glyph data.
    Colrv0,

    /// OpenType COLR version 1 paint graph glyph data.
    Colrv1,
}

impl Format {
    /// Parse a glyph payload format name.
    pub fn init(value: &[u8]) -> Option<Format> {
        match value {
            b"glyf" => Some(Format::Glyf),
            b"colrv0" => Some(Format::Colrv0),
            b"colrv1" => Some(Format::Colrv1),
            _ => None,
        }
    }
}

/// Register command reply verbosity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Reply {
    /// Suppress both success and failure replies.
    None = 0,

    /// Emit replies for both success and failure cases.
    All = 1,

    /// Emit replies only for failure cases.
    Failures = 2,
}

impl Reply {
    /// Parse the register command reply mode from its single-digit encoding.
    pub fn init(value: &[u8]) -> Option<Reply> {
        if value.len() != 1 {
            return None;
        }
        match value[0] {
            b'0' => Some(Reply::None),
            b'1' => Some(Reply::All),
            b'2' => Some(Reply::Failures),
            _ => None,
        }
    }
}

/// Register command width override for terminal layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Width {
    /// One terminal cell.
    Narrow = 1,

    /// Two terminal cells.
    Wide = 2,
}

impl Width {
    /// Parse the register command width from its single-digit encoding.
    pub fn init(value: &[u8]) -> Option<Width> {
        if value.len() != 1 {
            return None;
        }
        match value[0] {
            b'1' => Some(Width::Narrow),
            b'2' => Some(Width::Wide),
            _ => None,
        }
    }
}

/// Register command glyph scale policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    Height,
    Advance,
    Contain,
    Cover,
    Stretch,
}

impl Size {
    /// Parse a glyph scale policy name.
    pub fn init(value: &[u8]) -> Option<Size> {
        match value {
            b"height" => Some(Size::Height),
            b"advance" => Some(Size::Advance),
            b"contain" => Some(Size::Contain),
            b"cover" => Some(Size::Cover),
            b"stretch" => Some(Size::Stretch),
            _ => None,
        }
    }
}

/// Register command glyph placement within the render span.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Align {
    pub horizontal: Horizontal,
    pub vertical: Vertical,
}

impl Default for Align {
    fn default() -> Self {
        Align {
            horizontal: Horizontal::Center,
            vertical: Vertical::Center,
        }
    }
}

/// `Align.Horizontal`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Horizontal {
    Start,
    Center,
    End,
}

impl Horizontal {
    fn init(value: &[u8]) -> Option<Horizontal> {
        match value {
            b"start" => Some(Horizontal::Start),
            b"center" => Some(Horizontal::Center),
            b"end" => Some(Horizontal::End),
            _ => None,
        }
    }
}

/// `Align.Vertical`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vertical {
    Start,
    Center,
    End,
    Baseline,
}

impl Vertical {
    fn init(value: &[u8]) -> Option<Vertical> {
        match value {
            b"start" => Some(Vertical::Start),
            b"center" => Some(Vertical::Center),
            b"end" => Some(Vertical::End),
            b"baseline" => Some(Vertical::Baseline),
            _ => None,
        }
    }
}

impl Align {
    /// Parse an align value in `<horizontal>,<vertical>` form.
    pub fn init(value: &[u8]) -> Option<Align> {
        let mut it = value.split(|&c| c == b',');
        let horizontal = Horizontal::init(it.next()?)?;
        let vertical = Vertical::init(it.next()?)?;
        if it.next().is_some() {
            return None;
        }

        Some(Align {
            horizontal,
            vertical,
        })
    }
}

/// Register command fractional insets from the render span edges.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Pad {
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    pub left: f64,
}

impl Pad {
    /// Parse a pad value in `<top>,<right>,<bottom>,<left>` form.
    pub fn init(value: &[u8]) -> Option<Pad> {
        let mut it = value.split(|&c| c == b',');
        let top = Self::parse_fraction(it.next()?)?;
        let right = Self::parse_fraction(it.next()?)?;
        let bottom = Self::parse_fraction(it.next()?)?;
        let left = Self::parse_fraction(it.next()?)?;
        if it.next().is_some() {
            return None;
        }

        // Glyph Protocol §8.5.2: "If `l + r ≥ 1` or `t + b ≥ 1`
        // the terminal MUST treat the request as if `pad=0,0,0,0`."
        if left + right >= 1.0 || top + bottom >= 1.0 {
            return Some(Pad::default());
        }

        Some(Pad {
            top,
            right,
            bottom,
            left,
        })
    }

    /// Parse one pad component from the spec's `0.0`–`1.0` fractional range.
    /// Top/bottom fractions are relative to cell height; left/right fractions
    /// are relative to render span width.
    fn parse_fraction(value: &[u8]) -> Option<f64> {
        fraction::parse(value)
    }
}

/// Find the last occurrence of `key=value` for a lazily-parsed option list.
fn option_value<'a>(raw: &'a [u8], key: &[u8]) -> Option<&'a [u8]> {
    let mut remaining = raw;
    let mut result: Option<&[u8]> = None;
    while !remaining.is_empty() {
        // Options are semicolon-delimited, so each loop peels off one segment
        // and checks whether it matches the requested key.
        let len = memchr::memchr(b';', remaining).unwrap_or(remaining.len());
        let full = &remaining[..len];

        if let Some(eql_idx) = memchr::memchr(b'=', full) {
            if &full[..eql_idx] == key {
                result = Some(&full[eql_idx + 1..]);
            }
        }

        if len == remaining.len() {
            break;
        }
        remaining = &remaining[len + 1..];
    }

    result
}

/// `std.fmt.parseInt(T, buf, radix) catch null` for an unsigned `T` whose
/// maximum value is `max`: optional leading `+`/`-` (a negative non-zero
/// value overflows), `_` separators between digits, digits of either case.
fn parse_int(buf: &[u8], radix: u32, max: u64) -> Option<u64> {
    if buf.is_empty() {
        return None;
    }
    let (neg, digits) = match buf[0] {
        b'+' => (false, &buf[1..]),
        b'-' => (true, &buf[1..]),
        _ => (false, buf),
    };
    if digits.is_empty() || digits[0] == b'_' || digits[digits.len() - 1] == b'_' {
        return None;
    }

    let mut x: u64 = 0;
    for &c in digits {
        if c == b'_' {
            continue;
        }
        let digit = (c as char).to_digit(radix)? as u64;
        if x != 0 {
            x = x.checked_mul(radix as u64)?;
            if x > max {
                return None;
            }
        }
        x = if neg {
            x.checked_sub(digit)?
        } else {
            x.checked_add(digit)?
        };
        if x > max {
            return None;
        }
    }
    Some(x)
}

/// `std.base64.standard.Decoder` error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Base64Error {
    InvalidCharacter,
    InvalidPadding,
    NoSpaceLeft,
}

const BASE64_PAD: u8 = b'=';

fn base64_char_to_index(c: u8) -> Option<u8> {
    match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// `std.base64.standard.Decoder.calcSizeForSlice`.
fn base64_calc_size_for_slice(source: &[u8]) -> Result<usize, Base64Error> {
    let source_len = source.len();
    // calcSizeUpperBound with a pad char.
    if source_len % 4 != 0 {
        return Err(Base64Error::InvalidPadding);
    }
    let mut result = source_len / 4 * 3;
    if source_len >= 1 && source[source_len - 1] == BASE64_PAD {
        result -= 1;
    }
    if source_len >= 2 && source[source_len - 2] == BASE64_PAD {
        result -= 1;
    }
    Ok(result)
}

/// `std.base64.standard.Decoder.decode`.
fn base64_decode(dest: &mut [u8], source: &[u8]) -> Result<(), Base64Error> {
    if source.len() % 4 != 0 {
        return Err(Base64Error::InvalidPadding);
    }

    let mut dest_idx: usize = 0;
    // u12 accumulator
    let mut acc: u16 = 0;
    let mut acc_len: u8 = 0;
    let mut leftover_idx: Option<usize> = None;
    for (src_idx, &c) in source.iter().enumerate() {
        let Some(d) = base64_char_to_index(c) else {
            if c != BASE64_PAD {
                return Err(Base64Error::InvalidCharacter);
            }
            leftover_idx = Some(src_idx);
            break;
        };
        acc = ((acc << 6) & 0xFFF) + d as u16;
        acc_len += 6;
        if acc_len >= 8 {
            acc_len -= 8;
            if dest_idx >= dest.len() {
                return Err(Base64Error::NoSpaceLeft);
            }
            dest[dest_idx] = (acc >> acc_len) as u8;
            dest_idx += 1;
        }
    }
    if acc_len > 4 || (acc & ((1u16 << acc_len) - 1)) != 0 {
        return Err(Base64Error::InvalidPadding);
    }
    let Some(leftover_idx) = leftover_idx else {
        return Ok(());
    };
    let leftover = &source[leftover_idx..];
    let padding_len = (acc_len / 2) as usize;
    let mut padding_chars: usize = 0;
    for &c in leftover {
        if c != BASE64_PAD {
            return Err(if base64_char_to_index(c).is_none() {
                Base64Error::InvalidCharacter
            } else {
                Base64Error::InvalidPadding
            });
        }
        padding_chars += 1;
    }
    if padding_chars != padding_len {
        return Err(Base64Error::InvalidPadding);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::register_option as ro;
    use super::*;

    fn test_parse(data: &[u8]) -> Result<Request, Error> {
        let mut parser = CommandParser::init(1024 * 1024);
        for &byte in data {
            parser.feed(byte)?;
        }
        parser.complete()
    }

    fn register(cmd: &Request) -> &Register {
        match cmd {
            Request::Register(r) => r,
            other => panic!("expected register, got {other:?}"),
        }
    }

    #[test]
    fn support_command() {
        let cmd = test_parse(b"s").unwrap();
        assert_eq!(Request::Support, cmd);
    }

    #[test]
    fn query_command() {
        let cmd = test_parse(b"q;cp=E0A0").unwrap();
        let Request::Query(q) = &cmd else {
            panic!("expected query");
        };
        assert_eq!(0xE0A0u32, q.get(query_option::Cp).unwrap());
    }

    #[test]
    fn register_command_with_payload() {
        let cmd = test_parse(b"r;cp=e0a0;fmt=glyf;upm=1000;reply=2;QQ==").unwrap();
        let r = register(&cmd);
        assert_eq!(0xE0A0u32, r.get(ro::Cp).unwrap());
        assert_eq!(Format::Glyf, r.get(ro::Fmt).unwrap());
        assert_eq!(1000u32, r.get(ro::Upm).unwrap());
        assert_eq!(Reply::Failures, r.get(ro::Reply).unwrap());
        assert_eq!(b"QQ==", r.payload());
    }

    #[test]
    fn register_command_with_sizing_and_placement_options() {
        let cmd = test_parse(
            b"r;cp=e0a0;upm=2048;aw=1024;lh=1536;width=2;size=contain;align=end,baseline;pad=0.1,0.2,0.3,0.4;QQ==",
        )
        .unwrap();
        let r = register(&cmd);
        assert_eq!(2048u32, r.get(ro::Upm).unwrap());
        assert_eq!(1024u32, r.get(ro::Aw).unwrap());
        assert_eq!(1536u32, r.get(ro::Lh).unwrap());
        assert_eq!(Width::Wide, r.get(ro::Width).unwrap());
        assert_eq!(Size::Contain, r.get(ro::Size).unwrap());
        assert_eq!(
            Align {
                horizontal: Horizontal::End,
                vertical: Vertical::Baseline,
            },
            r.get(ro::Align).unwrap()
        );
        assert_eq!(
            Pad {
                top: 0.1,
                right: 0.2,
                bottom: 0.3,
                left: 0.4,
            },
            r.get(ro::Pad).unwrap()
        );
        assert_eq!(b"QQ==", r.payload());
    }

    #[test]
    fn register_option_defaults() {
        assert!(ro::Cp.default().is_none());
        assert_eq!(Format::Glyf, ro::Fmt.default().unwrap());
        assert_eq!(1000u32, ro::Upm.default().unwrap());
        assert!(ro::Aw.default().is_none());
        assert!(ro::Lh.default().is_none());
        assert_eq!(Width::Narrow, ro::Width.default().unwrap());
        assert_eq!(Size::Height, ro::Size.default().unwrap());
        assert_eq!(Align::default(), ro::Align.default().unwrap());
        assert_eq!(Pad::default(), ro::Pad.default().unwrap());
        assert_eq!(Reply::All, ro::Reply.default().unwrap());
    }

    #[test]
    fn register_command_defaults() {
        let cmd = test_parse(b"r;cp=e0a0;QQ==").unwrap();
        let r = register(&cmd);
        assert_eq!(0xE0A0u32, r.get(ro::Cp).unwrap());
        assert_eq!(Format::Glyf, r.get(ro::Fmt).unwrap());
        assert_eq!(1000u32, r.get(ro::Upm).unwrap());
        assert_eq!(1000u32, r.get(ro::Aw).unwrap());
        assert_eq!(1000u32, r.get(ro::Lh).unwrap());
        assert_eq!(Width::Narrow, r.get(ro::Width).unwrap());
        assert_eq!(Size::Height, r.get(ro::Size).unwrap());
        assert_eq!(Align::default(), r.get(ro::Align).unwrap());
        assert_eq!(Pad::default(), r.get(ro::Pad).unwrap());
        assert_eq!(Reply::All, r.get(ro::Reply).unwrap());
    }

    #[test]
    fn register_command_aw_and_lh_default_to_upm() {
        let cmd = test_parse(b"r;cp=e0a0;upm=2048;QQ==").unwrap();
        let r = register(&cmd);
        assert_eq!(2048u32, r.get(ro::Aw).unwrap());
        assert_eq!(2048u32, r.get(ro::Lh).unwrap());
    }

    #[test]
    fn register_command_invalid_sizing_and_placement_options() {
        let cmd = test_parse(
            b"r;cp=e0a0;width=3;size=invalid;align=center,middle;pad=0,1.2,0,0;QQ==",
        )
        .unwrap();
        let r = register(&cmd);
        assert!(r.get(ro::Width).is_none());
        assert!(r.get(ro::Size).is_none());
        assert!(r.get(ro::Align).is_none());
        assert!(r.get(ro::Pad).is_none());
    }

    #[test]
    fn register_command_degenerate_padding_defaults_to_no_padding() {
        let cmd = test_parse(b"r;cp=e0a0;pad=0.4,0.2,0.6,0.1;QQ==").unwrap();
        let r = register(&cmd);
        assert_eq!(Pad::default(), r.get(ro::Pad).unwrap());
    }

    #[test]
    fn register_command_invalid_reply_falls_back_to_reply_1() {
        let cmd = test_parse(b"r;cp=e0a0;reply=9;QQ==").unwrap();
        let r = register(&cmd);
        assert_eq!(Reply::All, r.get(ro::Reply).unwrap());
    }

    #[test]
    fn register_command_duplicate_options_use_the_last_value() {
        let cmd = test_parse(b"r;cp=e0a0;reply=1;reply=2;QQ==").unwrap();
        let r = register(&cmd);
        assert_eq!(Reply::Failures, r.get(ro::Reply).unwrap());
    }

    #[test]
    fn register_command_with_invalid_payload() {
        let cmd = test_parse(b"r;cp=e0a0;fmt=glyf;%%%not-base64%%%").unwrap();
        let r = register(&cmd);
        assert_eq!(0xE0A0u32, r.get(ro::Cp).unwrap());
        assert_eq!(Format::Glyf, r.get(ro::Fmt).unwrap());
        assert_eq!(b"%%%not-base64%%%", r.payload());
    }

    #[test]
    fn register_command_rejects_missing_payload_separator() {
        for data in [b"r".as_slice(), b"r;cp=e0a0", b"r;foo"] {
            assert_eq!(Err(Error::InvalidFormat), test_parse(data));
        }
    }

    #[test]
    fn register_decodes_glyf_payload() {
        let cmd = test_parse(b"r;cp=e0a0;AAAAAAAAAAAAAA==").unwrap();
        let outline = register(&cmd).decode_glyf_payload(usize::MAX).unwrap();
        assert_eq!(0, outline.points.len());
        assert_eq!(0, outline.contours.len());
    }

    #[test]
    fn register_rejects_malformed_glyf_payload() {
        let cmd = test_parse(b"r;cp=e0a0;%%%not-base64%%%").unwrap();
        assert_eq!(
            Err(DecodeError::MalformedPayload),
            register(&cmd).decode_glyf_payload(usize::MAX)
        );
    }

    #[test]
    fn register_response_without_payload() {
        let cmd = test_parse(b"r;cp=E0A0;status=4;reason=out_of_namespace").unwrap();

        // Register parsing is request-only, so the final segment is always treated
        // as payload rather than as a response field.
        let r = register(&cmd);
        assert_eq!(0xE0A0u32, r.get(ro::Cp).unwrap());
        assert_eq!(b"reason=out_of_namespace", r.payload());
    }

    #[test]
    fn clear_command() {
        let cmd = test_parse(b"c;cp=e0a0").unwrap();
        let Request::Clear(c) = &cmd else {
            panic!("expected clear");
        };
        assert!(c.has(clear_option::Cp));
        assert_eq!(0xE0A0u32, c.get(clear_option::Cp).unwrap());
    }

    #[test]
    fn clear_command_tracks_malformed_cp_presence() {
        for data in [b"c;cp=zz".as_slice(), b"c;cp=", b"c;cp=200000"] {
            let cmd = test_parse(data).unwrap();
            let Request::Clear(c) = &cmd else {
                panic!("expected clear");
            };
            assert!(c.has(clear_option::Cp));
            assert!(c.get(clear_option::Cp).is_none());
        }
    }

    #[test]
    fn invalid_command() {
        assert_eq!(Err(Error::InvalidFormat), test_parse(b"x"));
    }
}
