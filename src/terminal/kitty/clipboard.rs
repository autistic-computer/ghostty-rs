//! Port of `src/terminal/kitty/clipboard.zig`.
//!
//! STATUS: ported
//!
//! Kitty clipboard protocol (OSC 5522).
//!
//! This implements the protocol semantics on top of the raw OSC capture:
//! src/terminal/osc/parsers/kitty_clipboard_protocol.zig:
//!
//! The behavior here is modeled on the kitty reference implementation
//! (kitty/clipboard.py) rather than only the prose spec, since the two
//! disagree in places. Notable reference behaviors we reproduce:
//!
//!   * Malformed metadata (any record without '=', including an empty
//!     metadata section) and an unknown or missing `type` silently drop
//!     the request with no response.
//!   * All base64 (payloads and the `mime`, `name`, and `pw` metadata
//!     values) is strict RFC 4648 per the spec's "Encoding of payloads"
//!     section: characters outside the standard alphabet (including
//!     whitespace) and incorrect padding are rejected, never silently
//!     skipped. An invalid value on `wdata` or `walias` aborts an
//!     in-flight write with EINVAL; an invalid `read` is dropped with
//!     no response since reads have no error status.
//!   * Decoded metadata and MIME-list payloads must be valid UTF-8. An
//!     invalid value on `wdata` or `walias`, or a `walias` without a
//!     target MIME type, aborts an in-flight write with EINVAL.
//!   * The `wdata` payloads for one MIME type form a single base64
//!     stream split at arbitrary packet boundaries; only the
//!     concatenation must be correctly padded. Like kitty (which
//!     resets its streaming decoder on EOF), a packet ending exactly
//!     at terminal padding restarts the stream, so independently
//!     encoded chunks also work.
//!   * `mime`, `name`, and `pw` metadata values are base64-encoded UTF-8;
//!     everything else is verbatim. Unknown keys are ignored.
//!   * `id` is sanitized by stripping characters outside [a-zA-Z0-9-_+.]
//!     and truncating to 512 bytes, then echoed verbatim in every
//!     response packet (omitted when empty).
//!   * A `type=write` silently replaces any in-flight transaction. A
//!     commit (`type=wdata` without a MIME type) with no in-flight
//!     transaction is silently ignored.
//!   * Responses never send a payload section for an empty payload,
//!     except the targets ('.') listing DATA packet which is always sent.
//!
//! Specification: https://sw.kovidgoyal.net/kitty/clipboard/

use crate::terminal::osc as oscpkg;
use crate::terminal::osc::parsers::kitty_clipboard_protocol as protocol;

use super::clipboard_command as command;
use super::clipboard_grants as grants;
use super::clipboard_response as response;
use super::clipboard_write as write;

pub use protocol::Operation;
pub use protocol::Osc as OSC;
pub use protocol::Status;
pub use oscpkg::Terminator;

pub use command::read_prompt_exempt;
pub use command::Metadata;
pub use command::Payload;
pub use command::MAX_ID_LEN;
pub use command::MAX_MIME_LEN;
pub use command::MAX_NAME_LEN;
pub use command::MAX_PW_LEN;

pub use write::Content;
pub use write::WriteState;
pub use write::MAX_WRITE_ALIASES;
pub use write::MAX_WRITE_MIMES;
pub use write::MAX_WRITE_SIZE;

pub use response::PasteEvent;
pub use response::ReadSuccess;
pub use response::Response;
pub use response::MAX_LISTING_MIMES;
pub use response::MAX_READ_MIMES;
pub use response::READ_CHUNK_SIZE;
pub use response::TARGETS_MIME;

pub use grants::generate_otp;
pub use grants::Grants;
pub use grants::OTP_LEN;
