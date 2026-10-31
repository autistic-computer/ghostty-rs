//! Port of `src/terminal/kitty/dnd_command.zig`.
//!
//! STATUS: ported

/// Decoded OSC 72 metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Metadata {
    /// Event type (`t`). None when the metadata had no `t` key; such
    /// commands parse successfully but are ignored, matching kitty.
    pub r#type: Option<EventType>,

    /// Chunking flag (`m`): true when more chunks follow.
    pub more: bool,

    /// Multiplexer client ID (`i`), echoed in every response so a
    /// terminal multiplexer can route responses to the correct client.
    pub client_id: u32,

    /// Operation (`o`): meaning depends on the event type, commonly
    /// 0=none/reject, 1=copy, 2=move, 3=copy or move.
    pub operation: u32,

    /// `x`, `y`, `X`, `Y` keys.
    pub cell_x: i32,
    pub cell_y: i32,
    pub pixel_x: i32,
    pub pixel_y: i32,
}

impl Metadata {
    /// Parse raw OSC 72 metadata (the part before the first `;`).
    /// Returns None when malformed; callers should ignore the command,
    /// matching kitty which rejects the entire command on any error.
    pub fn parse(raw: &[u8]) -> Option<Metadata> {
        let mut result = Metadata::default();
        let mut pos: usize = 0;
        // The `pos += 1` at the end of each iteration consumes the ':'
        // separating a field from the next; the body advances past the
        // field itself.
        while pos < raw.len() {
            // Single-character key.
            let key = raw[pos];
            pos += 1;
            match key {
                b't' | b'm' | b'i' | b'o' | b'x' | b'y' | b'X' | b'Y' => {}
                _ => return None,
            }

            // '=' separator.
            if pos >= raw.len() {
                return None;
            }
            if raw[pos] != b'=' {
                return None;
            }
            pos += 1;

            // Value.
            if pos >= raw.len() {
                return None;
            }
            match key {
                b't' => {
                    result.r#type = Some(EventType::from_int(raw[pos])?);
                    pos += 1;
                }

                b'm' | b'i' | b'o' => {
                    let v = Self::parse_unsigned(raw, &mut pos)?;
                    match key {
                        b'm' => result.more = v != 0,
                        b'i' => result.client_id = v,
                        b'o' => result.operation = v,
                        _ => unreachable!(),
                    }
                }

                b'x' | b'y' | b'X' | b'Y' => {
                    let negative = raw[pos] == b'-';
                    if negative {
                        pos += 1;
                    }
                    let unsigned = Self::parse_unsigned(raw, &mut pos)?;
                    // Matches kitty's cast of the u32 magnitude to i32,
                    // which wraps rather than erroring on overflow.
                    let magnitude = unsigned as i32;
                    let v = if negative {
                        0i32.wrapping_sub(magnitude)
                    } else {
                        magnitude
                    };
                    match key {
                        b'x' => result.cell_x = v,
                        b'y' => result.cell_y = v,
                        b'X' => result.pixel_x = v,
                        b'Y' => result.pixel_y = v,
                        _ => unreachable!(),
                    }
                }

                _ => unreachable!(),
            }

            // Values are separated by ':'.
            if pos >= raw.len() {
                break;
            }
            if raw[pos] != b':' {
                return None;
            }
            pos += 1;
        }

        Some(result)
    }

    /// Parse an unsigned decimal value at `pos`, advancing it. At most
    /// 10 digits and at most u32::MAX, matching kitty. Returns None
    /// when there are no digits or the value is too large.
    fn parse_unsigned(raw: &[u8], pos: &mut usize) -> Option<u32> {
        let start = *pos;
        let mut acc: u64 = 0;
        let mut i = start;
        while i < raw.len() && i < start + 10 {
            let d = raw[i].wrapping_sub(b'0');
            if d > 9 {
                break;
            }
            acc = acc * 10 + d as u64;
            i += 1;
        }
        if i == start {
            return None;
        }
        *pos = i;
        u32::try_from(acc).ok()
    }
}

/// The event type, i.e. values for the `t` metadata key. A single OSC 72
/// code is used for both directions of the protocol, so most types have
/// one meaning when received by the terminal from a client and another
/// when sent by the terminal to a client.
///
/// The `drop` and `request_response` types are only ever sent by the
/// terminal; kitty parses but ignores them when received and we do the
/// same.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum EventType {
    /// 'a': (recv) client registers to accept drops. With x=1 the payload
    /// is the client's machine ID for remote drop support instead.
    Register = b'a',

    /// 'A': (recv) client unregisters from accepting drops.
    Unregister = b'A',

    /// 'm': (recv) client reports acceptance status for the drag currently
    /// over the terminal: `o` is the chosen operation and the payload is
    /// the accepted MIME list. (send) pointer moved over the terminal
    /// during a drag, or with x=-1,y=-1 the drag left the window.
    Status = b'm',

    /// 'M': (send only) items were dropped onto the terminal.
    Drop = b'M',

    /// 'r': (recv) client requests drop data, or with x=y=Y=0 concludes
    /// the drop with `o` as the performed operation. (send) drop data
    /// response chunks.
    Request = b'r',

    /// 'R': (send only) error response to a data request.
    RequestError = b'R',

    /// 'o': (recv) drag source control: x=1 enables offering drags (payload
    /// optionally the client machine ID), x=2 disables, x=0 offers a MIME
    /// list for a new drag. (send) request that the client start a drag at
    /// the given position.
    Offer = b'o',

    /// 'p': (recv) pre-sent data for an offered drag: x>=0 is a 0-based
    /// MIME index, x<0 attaches drag image -x.
    Present = b'p',

    /// 'P': (recv) x=-1 starts the offered drag, x>=0 changes the drag
    /// image mid-drag.
    StartDrag = b'P',

    /// 'e': (recv) drag data for MIME index `y` of an in-progress drag.
    /// (send) drag status events (accepted, dropped, finished, ...).
    DragEvent = b'e',

    /// 'E': (recv) client aborts the whole drag (y=-1) or reports an error
    /// for MIME index `y`. (send) drag start response (OK or error).
    DragError = b'E',

    /// 'k': (recv) remote file data for a drag. (send) request for remote
    /// file data.
    RemoteData = b'k',

    /// 'q': (recv) query protocol support. (send) the query response.
    Query = b'q',
}

impl EventType {
    /// Convert from the integer value (`std.enums.fromInt`).
    pub fn from_int(v: u8) -> Option<EventType> {
        Some(match v {
            b'a' => Self::Register,
            b'A' => Self::Unregister,
            b'm' => Self::Status,
            b'M' => Self::Drop,
            b'r' => Self::Request,
            b'R' => Self::RequestError,
            b'o' => Self::Offer,
            b'p' => Self::Present,
            b'P' => Self::StartDrag,
            b'e' => Self::DragEvent,
            b'E' => Self::DragError,
            b'k' => Self::RemoteData,
            b'q' => Self::Query,
            _ => return None,
        })
    }
}

/// A drop operation. Values match the protocol's `o` key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum Operation {
    #[default]
    None = 0,
    Copy = 1,
    Move = 2,
}

impl Operation {
    /// Convert a protocol `o` value the way kitty does: anything other
    /// than copy or move means none.
    pub fn from_protocol(v: u32) -> Operation {
        match v {
            1 => Operation::Copy,
            2 => Operation::Move,
            _ => Operation::None,
        }
    }
}

/// The set of operations allowed by a drag source, sent as a bitmask in
/// the `o` key of move and drop events.
///
/// `packed struct(u2)`: bit 0 is `copy`, bit 1 is `move`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Operations {
    pub copy: bool,
    pub r#move: bool,
}

impl Operations {
    /// The u2 bitmask value.
    pub fn protocol_value(self) -> u8 {
        (self.copy as u8) | ((self.r#move as u8) << 1)
    }
}

/// A decoded `t=r` data request. The request form is disambiguated by
/// which keys are non-zero, mirroring kitty's drop_process_queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    /// x=y=Y=0: the drop is concluded with the given operation.
    Conclude(Operation),

    /// Y=0, y=0, x!=0: request data for the 1-based MIME index x.
    Mime(i32),

    /// Y=0, y!=0: request the contents of the y'th (1-based) file in the
    /// text/uri-list MIME at 1-based index x. Remote drops only.
    Uri { mime_idx: i32, uri_idx: i32 },

    /// Y!=0: request entry x (1-based) of directory handle Y, or close
    /// the handle when x=0. Remote drops only.
    Dir { handle: i32, entry: i32 },
}

impl Request {
    pub fn init(meta: Metadata) -> Request {
        if meta.pixel_y != 0 {
            return Request::Dir {
                handle: meta.pixel_y,
                entry: meta.cell_x,
            };
        }
        if meta.cell_y != 0 {
            return Request::Uri {
                mime_idx: meta.cell_x,
                uri_idx: meta.cell_y,
            };
        }
        if meta.cell_x != 0 {
            return Request::Mime(meta.cell_x);
        }
        Request::Conclude(Operation::from_protocol(meta.operation))
    }
}

/// Chunk reassembly state.
///
/// While a chunked command is in progress, the metadata of the first
/// chunk is reused for all subsequent chunks; only the `more` flag is
/// taken from each continuation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Chunking {
    pub active: bool,
    pub metadata: Metadata,
}

impl Chunking {
    /// Returns the effective metadata for a received chunk and updates
    /// the reassembly state.
    pub fn apply(&mut self, meta: Metadata) -> Metadata {
        if self.active {
            let mut copy = self.metadata;
            copy.more = meta.more;
            self.active = meta.more;
            return copy;
        }

        if meta.more {
            self.active = true;
            self.metadata = meta;
        }

        meta
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_empty() {
        let meta = Metadata::parse(b"").unwrap();
        assert!(meta.r#type.is_none());
        assert!(!meta.more);
        assert_eq!(0u32, meta.client_id);
    }

    #[test]
    fn metadata_all_keys() {
        let meta = Metadata::parse(b"t=m:m=1:i=3:o=2:x=10:y=5:X=320:Y=200").unwrap();
        assert_eq!(EventType::Status, meta.r#type.unwrap());
        assert!(meta.more);
        assert_eq!(3u32, meta.client_id);
        assert_eq!(2u32, meta.operation);
        assert_eq!(10i32, meta.cell_x);
        assert_eq!(5i32, meta.cell_y);
        assert_eq!(320i32, meta.pixel_x);
        assert_eq!(200i32, meta.pixel_y);
    }

    #[test]
    fn metadata_all_event_types() {
        let cases: &[(&[u8], EventType)] = &[
            (b"t=a", EventType::Register),
            (b"t=A", EventType::Unregister),
            (b"t=m", EventType::Status),
            (b"t=M", EventType::Drop),
            (b"t=r", EventType::Request),
            (b"t=R", EventType::RequestError),
            (b"t=o", EventType::Offer),
            (b"t=p", EventType::Present),
            (b"t=P", EventType::StartDrag),
            (b"t=e", EventType::DragEvent),
            (b"t=E", EventType::DragError),
            (b"t=k", EventType::RemoteData),
            (b"t=q", EventType::Query),
        ];
        for (input, expected) in cases {
            assert_eq!(*expected, Metadata::parse(input).unwrap().r#type.unwrap());
        }
    }

    #[test]
    fn metadata_case_sensitive_coordinate_keys() {
        let meta = Metadata::parse(b"x=10:Y=200").unwrap();
        assert_eq!(10i32, meta.cell_x);
        assert_eq!(0i32, meta.cell_y);
        assert_eq!(0i32, meta.pixel_x);
        assert_eq!(200i32, meta.pixel_y);
    }

    #[test]
    fn metadata_negative_coordinates() {
        let meta = Metadata::parse(b"t=m:x=-1:y=-1").unwrap();
        assert_eq!(-1i32, meta.cell_x);
        assert_eq!(-1i32, meta.cell_y);
    }

    #[test]
    fn metadata_malformed_inputs_rejected() {
        // Unknown event type.
        assert!(Metadata::parse(b"t=z").is_none());
        // Unknown key.
        assert!(Metadata::parse(b"z=1").is_none());
        // Missing '=' mid-stream.
        assert!(Metadata::parse(b"x10").is_none());
        // No digits.
        assert!(Metadata::parse(b"x=notanumber").is_none());
        assert!(Metadata::parse(b"x=-").is_none());
        // Too large.
        assert!(Metadata::parse(b"i=4294967296").is_none());
        assert!(Metadata::parse(b"i=99999999999").is_none());
        // Garbage after value.
        assert!(Metadata::parse(b"x=1z").is_none());
        // No whitespace tolerance, matching kitty.
        assert!(Metadata::parse(b"t=a: x=1").is_none());
        assert!(Metadata::parse(b"t = a").is_none());
        // Truncated mid-construct, matching kitty's final-state check.
        // Nothing state-changing may be parsed out of these: e.g. "t=r:x="
        // must not be treated as a drop conclusion.
        assert!(Metadata::parse(b"t").is_none());
        assert!(Metadata::parse(b"t=").is_none());
        assert!(Metadata::parse(b"x=").is_none());
        assert!(Metadata::parse(b"t=r:x=").is_none());
        assert!(Metadata::parse(b"x=1:t=").is_none());
    }

    #[test]
    fn metadata_trailing_separator_accepted() {
        // Kitty's state machine accepts the metadata ending right after a
        // value separator.
        let meta = Metadata::parse(b"t=a:").unwrap();
        assert_eq!(EventType::Register, meta.r#type.unwrap());
    }

    #[test]
    fn metadata_u32_boundary_accepted() {
        let meta = Metadata::parse(b"i=4294967295").unwrap();
        assert_eq!(u32::MAX, meta.client_id);
    }

    #[test]
    fn request_classification() {
        // Conclude.
        {
            let r = Request::init(Metadata::parse(b"t=r:o=2").unwrap());
            assert_eq!(Request::Conclude(Operation::Move), r);
        }
        // Conclude with unknown operation is none (canceled).
        {
            let r = Request::init(Metadata::parse(b"t=r:o=9").unwrap());
            assert_eq!(Request::Conclude(Operation::None), r);
        }
        // MIME data request.
        {
            let r = Request::init(Metadata::parse(b"t=r:x=2").unwrap());
            assert_eq!(Request::Mime(2), r);
        }
        // URI file request.
        {
            let r = Request::init(Metadata::parse(b"t=r:x=1:y=3").unwrap());
            assert_eq!(
                Request::Uri {
                    mime_idx: 1,
                    uri_idx: 3
                },
                r
            );
        }
        // Directory handle request.
        {
            let r = Request::init(Metadata::parse(b"t=r:Y=2:x=1").unwrap());
            assert_eq!(
                Request::Dir {
                    handle: 2,
                    entry: 1
                },
                r
            );
        }
    }

    #[test]
    fn chunking_reassembly_reuses_first_chunk_metadata() {
        let mut chunking = Chunking::default();

        // First chunk starts reassembly.
        let first = chunking.apply(Metadata::parse(b"t=m:o=1:m=1").unwrap());
        assert!(chunking.active);
        assert_eq!(EventType::Status, first.r#type.unwrap());
        assert!(first.more);

        // Continuation metadata is ignored except for `more`.
        let second = chunking.apply(Metadata::parse(b"t=q:o=2:m=1").unwrap());
        assert!(chunking.active);
        assert_eq!(EventType::Status, second.r#type.unwrap());
        assert_eq!(1u32, second.operation);
        assert!(second.more);

        // Final chunk ends reassembly.
        let last = chunking.apply(Metadata::parse(b"t=q:m=0").unwrap());
        assert!(!chunking.active);
        assert_eq!(EventType::Status, last.r#type.unwrap());
        assert!(!last.more);
    }

    #[test]
    fn chunking_unchunked_commands_pass_through() {
        let mut chunking = Chunking::default();
        let meta = chunking.apply(Metadata::parse(b"t=q").unwrap());
        assert!(!chunking.active);
        assert_eq!(EventType::Query, meta.r#type.unwrap());
    }

    #[test]
    fn operations_protocol_value() {
        assert_eq!(0, Operations::default().protocol_value());
        assert_eq!(
            3,
            Operations {
                copy: true,
                r#move: true
            }
            .protocol_value()
        );
        assert_eq!(
            2,
            Operations {
                copy: false,
                r#move: true
            }
            .protocol_value()
        );
    }
}
