//! Port of `src/terminal/kitty/dnd_drop.zig`.
//!
//! STATUS: ported
//!
//! Kitty drag and drop protocol (OSC 72) state machine.
//!
//! Deviations: the `*?*State` slot is `&mut Option<Box<State>>`;
//! `std.Io.Writer` is any `std::io::Write`; the held drop data is owned
//! (`HeldItem`) while `Item` is the borrowed form embedders pass to
//! `State::drag_drop`.

use std::io::Write;
use std::ops::Range;

use crate::terminal::osc;

use super::dnd_command as command;
use super::dnd_response as response;

use command::{EventType, Metadata, Operation, Operations};

/// Maximum accumulated size of a client-sent MIME list (the accepted
/// list of a `t=m` status update). Matches kitty's MIME_LIST_SIZE_CAP.
pub const MAX_MIME_LIST_BYTES: usize = 1024 * 1024;

/// Process one OSC 72 command received from the client, writing any
/// responses to the writer. Returns the state change the embedder may
/// need to act on, if any.
pub fn handle_command<W: Write + ?Sized>(
    slot: &mut Option<Box<State>>,
    writer: &mut W,
    v: osc::KittyDndProtocol<'_>,
) -> std::io::Result<Option<Event>> {
    let Some(raw) = Metadata::parse(v.metadata) else {
        log::debug!(target: "kitty_dnd", "dropping malformed OSC 72 metadata");
        return Ok(None);
    };

    // Chunk reassembly lives in the state, so before registration
    // each command stands alone. The only legitimately chunked
    // command before registration is t=a itself, which seeds the
    // reassembly on its first chunk below.
    let continuation = slot.as_ref().is_some_and(|state| state.chunking.active);
    let meta = match slot.as_mut() {
        Some(state) => state.chunking.apply(raw),
        None => raw,
    };
    let payload: &[u8] = v.payload.unwrap_or(b"");
    let Some(t) = meta.r#type else {
        return Ok(None);
    };

    match t {
        EventType::Register => {
            // x=1 declares the client's machine ID for remote drop
            // support. We don't support remote drop yet, so accept and ignore.
            if meta.cell_x == 1 {
                return Ok(None);
            }

            // Setup our state if we haven't already
            let state = slot.get_or_insert_with(|| {
                let mut state = State::create();
                let _ = state.chunking.apply(raw);
                state
            });

            // Update the client ID on every registration
            state.drop.client_id = meta.client_id;

            return Ok(state.register(payload, continuation, meta.more));
        }

        EventType::Unregister => {
            let Some(state) = slot.take() else {
                return Ok(None);
            };
            state.destroy();
            return Ok(Some(Event::Registration));
        }

        EventType::Status => {
            let Some(state) = slot.as_mut() else {
                return Ok(None);
            };
            return Ok(state.accept_status(meta, payload));
        }

        EventType::Request => {
            return data_request(slot.as_deref_mut(), writer, meta, v.terminator);
        }

        // Drag source control. Enabling (x=1, with an optional
        // machine ID payload) and disabling (x=2) offers are
        // accepted and ignored since the terminal never requests a
        // drag start. Offering a MIME list (x=0) for a new drag is
        // refused since drag-out is not implemented.
        EventType::Offer => {
            if meta.cell_x == 0 {
                refuse_drag_out(writer, meta, v.terminator)?;
            }
        }

        // Drag-out data and start commands. A conforming client
        // never sends these because the terminal never requests a
        // drag start, but refuse them properly if one does.
        EventType::Present | EventType::StartDrag => {
            refuse_drag_out(writer, meta, v.terminator)?;
        }

        // Responses to drag-out requests the terminal never makes.
        EventType::DragEvent | EventType::DragError | EventType::RemoteData => {}

        EventType::Query => response::encode(
            writer,
            b"t=q",
            meta.client_id,
            b"",
            response::Encoding::Plain,
            v.terminator,
        )?,

        // Only ever sent by the terminal. Ignore.
        EventType::Drop | EventType::RequestError => {}
    }

    Ok(None)
}

/// Handle a t=r data request or drop conclusion from the client.
/// Requests from an unregistered client (no state) get the same
/// errors kitty sends from its zeroed drop state.
fn data_request<W: Write + ?Sized>(
    state: Option<&mut State>,
    writer: &mut W,
    meta: Metadata,
    terminator: osc::Terminator,
) -> std::io::Result<Option<Event>> {
    // Responses echo the registration's client ID, matching kitty.
    let client_id = state.as_ref().map_or(0, |s| s.drop.client_id);

    match command::Request::init(meta) {
        command::Request::Conclude(op) => {
            // The client is done with the drop: free the held data and
            // report the operation it performed. Kitty hands that to
            // the still-open OS drag session; ours ended at drop time
            // (see dnd.zig), so the embedder decides what to do with
            // it. A conclusion with no drop in progress is a no-op.
            let Some(s) = state else {
                return Ok(None);
            };
            let dropped = s.drop.dropped;
            s.reset_drop();
            return Ok(if dropped { Some(Event::concluded(op)) } else { None });
        }

        command::Request::Mime(idx) => {
            let keys = response::RequestKeys {
                x: idx,
                ..Default::default()
            };
            let Some(items) = state.as_ref().and_then(|s| s.drop.items.as_ref()) else {
                response::encode_error(
                    writer,
                    response::ErrorKind::Drop,
                    keys,
                    client_id,
                    response::Errno::ENOENT,
                    b"no drop data available",
                    terminator,
                )?;
                return Ok(None);
            };
            if idx < 1 || idx as usize > items.len() {
                response::encode_error(
                    writer,
                    response::ErrorKind::Drop,
                    keys,
                    client_id,
                    response::Errno::ENOENT,
                    b"drop data request index out of bounds",
                    terminator,
                )?;
                return Ok(None);
            }

            let header = format!("t=r{keys}");

            // The data chunks followed by the empty end-of-data
            // message, which is how the client detects completion.
            // An empty item is just the end-of-data message alone;
            // clients treat a duplicate as a second completion.
            let item = &items[(idx - 1) as usize];
            if !item.data.is_empty() {
                response::encode(
                    writer,
                    header.as_bytes(),
                    client_id,
                    &item.data,
                    response::Encoding::Base64,
                    terminator,
                )?;
            }
            response::encode(
                writer,
                header.as_bytes(),
                client_id,
                b"",
                response::Encoding::Base64,
                terminator,
            )?;
            return Ok(None);
        }

        // Remote drop transfers (URI file contents and directory
        // handles). We never advertise remote support (no X=1
        // marker), so a conforming client never sends these.
        command::Request::Uri { mime_idx, uri_idx } => response::encode_error(
            writer,
            response::ErrorKind::Drop,
            response::RequestKeys {
                x: mime_idx,
                y: uri_idx,
                Y: 0,
            },
            client_id,
            response::Errno::EINVAL,
            b"remote drop data is not supported",
            terminator,
        )?,
        command::Request::Dir { handle, entry } => response::encode_error(
            writer,
            response::ErrorKind::Drop,
            response::RequestKeys {
                x: entry,
                y: 0,
                Y: handle,
            },
            client_id,
            response::Errno::EINVAL,
            b"remote drop data is not supported",
            terminator,
        )?,
    }

    Ok(None)
}

/// Refuse a drag-out command with an error, since ghostty does not
/// implement the terminal side of client-initiated drags yet.
fn refuse_drag_out<W: Write + ?Sized>(
    writer: &mut W,
    meta: Metadata,
    terminator: osc::Terminator,
) -> std::io::Result<()> {
    response::encode_error(
        writer,
        response::ErrorKind::Drag,
        response::RequestKeys::default(),
        meta.client_id,
        response::Errno::EPERM,
        b"drag out is not supported by this terminal",
        terminator,
    )
}

/// A protocol state change an embedder may need to act on, returned by
/// `handle_command` and delivered through the stream handler's
/// `drag_and_drop` effect. This is a flat enum so it can cross a C API
/// unchanged; any details are read back from `Terminal.kitty_dnd`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Event {
    /// The client registered (t=a), re-registered, or unregistered
    /// (t=A) to accept drops. An embedder may want to use this
    /// to setup the proper mime types to accept (e.g. on macOS)
    /// or not (unregistered).
    Registration,

    /// The client answered the drag currently over the terminal.
    /// `State::client_accepted` has the answer. Embedders can refresh the
    /// OS drag feedback immediately rather than on the next move.
    Acceptance,

    /// The client concluded a drop, performing no operation (it
    /// canceled), a copy, or a move. The held drop data has been freed.
    ConcludedNone,
    ConcludedCopy,
    ConcludedMove,
}

impl Event {
    /// The conclusion event for a performed operation.
    pub fn concluded(op: Operation) -> Event {
        match op {
            Operation::None => Event::ConcludedNone,
            Operation::Copy => Event::ConcludedCopy,
            Operation::Move => Event::ConcludedMove,
        }
    }
}

/// The per-terminal drop target state.
///
/// The primary entrypoint is `handle_command` which takes a
/// `&mut Option<Box<State>>` slot that it can heap allocate into when
/// DnD activates and free when it deactivates.
///
/// The normal lifecycle:
///
///   1. The stream handler feeds every OSC 72 command received from the
///      client to `handle_command`. The client registers (t=a), which
///      allocates the state into the slot and yields a `registration`
///      event so the embedder can register any declared MIME types
///      with the OS. Until then `handle_command` only answers stateless
///      commands (queries, error responses).
///   2. A native drag enters or moves over the terminal. When the slot
///      is non-null, the embedder calls `drag_move` with the pointer
///      position, the operations the drag source allows, and the MIME
///      types it can serve if dropped. This sends the client a t=m
///      move event; when the slot is null the embedder should handle
///      the drag as it would without the protocol.
///   3. The client answers with its acceptance (t=m:o=N), recorded by
///      `handle_command` which yields an `acceptance` event. The
///      embedder reads `client_accepted` then and on subsequent moves
///      to give the OS drag session its feedback.
///   4. The drag either leaves, and the embedder calls `drag_leave` to
///      send the t=m leave event, or drops: the embedder captures the
///      representations it advertised and calls `drag_drop`, which
///      copies and holds them and sends the client a t=M drop event. A
///      new drag entering before the client concludes discards the
///      held drop.
///   5. The client requests data (t=r:x=N), which `handle_command`
///      serves from the held copies, and then concludes the drop
///      (t=r:o=N), which frees them and yields a `concluded_*` event
///      naming the operation the client performed.
///   6. The client unregisters (t=A) and `handle_command` frees the
///      state, yielding a final `registration` event, or the terminal
///      is deinitialized and calls `destroy`.
///
/// All calls require the same synchronization as any other terminal
/// mutation.
#[derive(Debug, Clone, Default)]
pub struct State {
    /// Chunk reassembly for client commands. This is the only part of
    /// the state cleared by a terminal reset (RIS), matching kitty.
    pub chunking: command::Chunking,

    /// Drop target state for the registered client.
    pub drop: DropTarget,
}

#[derive(Debug, Clone, Default)]
pub struct DropTarget {
    /// Multiplexer client ID from registration, echoed in every
    /// drop-side message the terminal sends.
    pub client_id: u32,

    /// The MIME list the client registered with (the t=a payload),
    /// space-separated as received and accumulated across chunks.
    /// Only needed by embedders that must register types with the
    /// OS ahead of a drag; kitty frees it after doing so, we keep
    /// it so the `registration` event can be acted on from here.
    pub registered_mimes: Vec<u8>,

    /// True while the pointer of a native drag is over the terminal.
    pub hovered: bool,

    /// True after the native drop until the client concludes it.
    pub dropped: bool,

    /// The client's response to the current drag, None until the
    /// client has responded. `None` (the operation) means the client
    /// rejected it.
    pub accepted: Option<Operation>,

    /// True while a chunked t=m acceptance is being accumulated.
    pub accept_in_progress: bool,

    /// The client's accepted MIME list: space-separated while
    /// accumulating, converted to NUL-separated (with a trailing
    /// NUL) once complete, matching kitty's in-place conversion.
    pub accepted_mimes: Vec<u8>,

    /// The MIME types of the current native drag, in the order
    /// that data request indices refer to.
    offered: Option<Offered>,

    /// The data captured at drop time, parallel to `offered`.
    pub items: Option<Vec<HeldItem>>,
}

/// One dropped representation: a MIME type and its data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Item<'a> {
    pub mime: &'a [u8],
    pub data: &'a [u8],
}

/// An owned copy of an [`Item`] held by the state until the drop
/// concludes (Zig: the copied `Item`s in `DropTarget.items`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldItem {
    pub mime: Vec<u8>,
    pub data: Vec<u8>,
}

/// The MIME list of the current drag plus the pre-joined move-event
/// payload ("mime1 mime2 " with a trailing space after every entry,
/// matching kitty) so per-move encoding is allocation-free.
#[derive(Debug, Clone)]
struct Offered {
    /// Ranges of `payload` holding each MIME type.
    mimes: Vec<Range<usize>>,
    payload: Vec<u8>,
}

impl Offered {
    fn init(mimes: &[&[u8]]) -> Offered {
        let payload_len: usize = mimes.iter().map(|m| m.len() + 1).sum();
        let mut payload: Vec<u8> = Vec::with_capacity(payload_len);
        let mut ranges: Vec<Range<usize>> = Vec::with_capacity(mimes.len());

        for m in mimes {
            let offset = payload.len();
            payload.extend_from_slice(m);
            payload.push(b' ');
            ranges.push(offset..offset + m.len());
        }

        Offered {
            mimes: ranges,
            payload,
        }
    }

    fn eql(&self, mimes: &[&[u8]]) -> bool {
        if self.mimes.len() != mimes.len() {
            return false;
        }
        self.mimes
            .iter()
            .zip(mimes)
            .all(|(a, b)| &self.payload[a.clone()] == *b)
    }
}

/// A native drag position report from the embedder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MoveEvent {
    /// Grid cell under the pointer, zero-based from the top-left.
    pub cell_x: u32,
    pub cell_y: u32,

    /// Pointer position in pixels relative to the top-left of the
    /// terminal's content area.
    pub pixel_x: i32,
    pub pixel_y: i32,

    /// The operations the drag source allows.
    pub operations: Operations,
}

impl State {
    /// The maximum number of dropped items. Embedders provide a small
    /// curated set of representations (see dnd.zig), so this is a
    /// generous bound that keeps the MIME list assembly on the stack.
    pub const MAX_ITEMS: usize = 16;

    /// Allocate a fresh state. Done by `handle_command` on registration.
    fn create() -> Box<State> {
        Box::default()
    }

    /// Free the state and everything it holds.
    pub fn destroy(self: Box<Self>) {
        drop(self);
    }

    /// Iterate the MIME types the client registered with, in order.
    /// Empty when the client declared none, which is the common case.
    /// The list is only needed to register exotic types with the OS,
    /// such as macOS pasteboard stuff.
    pub fn registered_mimes(&self) -> impl Iterator<Item = &[u8]> {
        self.drop
            .registered_mimes
            .split(|&c| c == b' ')
            .filter(|s| !s.is_empty())
    }

    /// Record one chunk of a registration's MIME list.
    ///
    /// `continuation` is true for every chunk but the first of a chunked
    /// registration. Returns the registration event once the list is complete.
    fn register(&mut self, payload: &[u8], continuation: bool, more: bool) -> Option<Event> {
        let list = &mut self.drop.registered_mimes;
        if !continuation {
            list.clear();
        }

        // Matching kitty, an over-cap chunk is dropped and does not
        // complete the registration.
        if list.len() + payload.len() > MAX_MIME_LIST_BYTES {
            return None;
        }
        list.extend_from_slice(payload);

        if more {
            None
        } else {
            Some(Event::Registration)
        }
    }

    /// The client's acceptance response for the drag currently over the
    /// terminal, for OS drag feedback. None when the client hasn't
    /// responded yet (embedders should fall back to their default,
    /// typically copy) or `Operation::None` when the client rejected the
    /// drag.
    pub fn client_accepted(&self) -> Option<Operation> {
        if self.drop.accept_in_progress {
            return None;
        }
        self.drop.accepted
    }

    /// Free the per-drag data (offered MIME list and held drop items).
    fn free_drag_data(&mut self) {
        self.drop.offered = None;
        self.drop.items = None;
    }

    /// Clear the per-drag state while preserving the registration,
    /// mirroring kitty's reset_drop. Called when a new drag enters and
    /// when a drop concludes.
    fn reset_drop(&mut self) {
        self.free_drag_data();
        self.drop.accepted_mimes = Vec::new();
        self.drop.hovered = false;
        self.drop.dropped = false;
        self.drop.accepted = None;
        self.drop.accept_in_progress = false;
    }

    /// Handle a t=m acceptance status update from the client, mirroring
    /// kitty's drop_set_status.
    fn accept_status(&mut self, meta: Metadata, payload: &[u8]) -> Option<Event> {
        let d = &mut self.drop;
        if !d.accept_in_progress {
            d.accepted_mimes.clear();
            d.accept_in_progress = true;
            d.accepted = Some(Operation::from_protocol(meta.operation));
        }

        if !payload.is_empty() {
            // Matching kitty, an over-cap list stops accumulating and
            // never finalizes, leaving the acceptance unanswered.
            if d.accepted_mimes.len() + payload.len() > MAX_MIME_LIST_BYTES {
                return None;
            }
            d.accepted_mimes.extend_from_slice(payload);
        }

        if meta.more {
            return None;
        }
        d.accept_in_progress = false;
        if !d.accepted_mimes.is_empty() {
            for c in d.accepted_mimes.iter_mut() {
                if *c == b' ' {
                    *c = 0;
                }
            }
            d.accepted_mimes.push(0);
        }
        Some(Event::Acceptance)
    }

    /// Report a native drag moving over the terminal, sending a t=m
    /// move event to the client. `mimes` is the list of MIME types the
    /// terminal can provide for this drag, in the order data request
    /// indices will refer to.
    pub fn drag_move<W: Write + ?Sized>(
        &mut self,
        writer: &mut W,
        ev: MoveEvent,
        mimes: &[&[u8]],
    ) -> std::io::Result<()> {
        self.move_event(writer, ev, mimes, false)
    }

    /// Report a native drop onto the terminal. The items' data is
    /// copied and held so the client's data requests can be served; it
    /// is freed when the client concludes the drop, a new drag enters,
    /// or the client unregisters.
    ///
    /// Sends a t=M drop event listing the items' MIME types.
    pub fn drag_drop<W: Write + ?Sized>(
        &mut self,
        writer: &mut W,
        ev: MoveEvent,
        items: &[Item<'_>],
    ) -> std::io::Result<()> {
        // Copy the items so they can be served after this call returns.
        // Items beyond the cap are dropped so the held list always
        // matches the advertised MIME list.
        let accepted_items = &items[..items.len().min(Self::MAX_ITEMS)];
        let copies: Vec<HeldItem> = accepted_items
            .iter()
            .map(|item| HeldItem {
                mime: item.mime.to_vec(),
                data: item.data.to_vec(),
            })
            .collect();

        // The move handling below resets per-drag state when this drop
        // arrives without a preceding move, so the items are attached
        // after it runs. Collect the MIME list first.
        let mimes: Vec<&[u8]> = copies.iter().map(|item| item.mime.as_slice()).collect();

        self.move_event(writer, ev, &mimes, true)?;

        debug_assert!(self.drop.items.is_none());
        self.drop.items = Some(copies);
        Ok(())
    }

    /// Report the native drag leaving the terminal, sending the t=m
    /// leave event (x=-1, y=-1).
    ///
    /// Ignored after a drop: some toolkits emit a leave notification
    /// for the drop itself, and the held data must survive until the
    /// client concludes.
    pub fn drag_leave<W: Write + ?Sized>(&mut self, writer: &mut W) -> std::io::Result<()> {
        if self.drop.dropped {
            return Ok(());
        }
        let hovered = self.drop.hovered;
        self.drop.hovered = false;
        self.drop.offered = None;

        // Only a client that saw the drag enter gets the leave event,
        // matching kitty which notifies hovered windows only.
        if !hovered {
            return Ok(());
        }

        response::encode(
            writer,
            b"t=m:x=-1:y=-1",
            self.drop.client_id,
            b"",
            response::Encoding::Plain,
            osc::Terminator::St,
        )
    }

    /// Shared implementation of move and drop events, mirroring kitty's
    /// drop_move_on_child.
    fn move_event<W: Write + ?Sized>(
        &mut self,
        writer: &mut W,
        ev: MoveEvent,
        mimes: &[&[u8]],
        is_drop: bool,
    ) -> std::io::Result<()> {
        if !self.drop.hovered {
            self.reset_drop();
            self.drop.hovered = true;
        }
        if is_drop {
            self.drop.dropped = true;
            self.drop.hovered = false;
        }

        // (Re)build the offered MIME list when it changed.
        if !self.drop.offered.as_ref().is_some_and(|o| o.eql(mimes)) {
            self.drop.offered = Some(Offered::init(mimes));
        }

        let header = format!(
            "t={}:x={}:y={}:X={}:Y={}:o={}",
            if is_drop { 'M' } else { 'm' },
            ev.cell_x,
            ev.cell_y,
            ev.pixel_x,
            ev.pixel_y,
            ev.operations.protocol_value(),
        );

        // The MIME list is sent with every move event, matching kitty
        // (the spec suggests only the first, but kitty always sends it
        // and clients depend on that).
        response::encode(
            writer,
            header.as_bytes(),
            self.drop.client_id,
            &self.drop.offered.as_ref().unwrap().payload,
            response::Encoding::Plain,
            osc::Terminator::St,
        )
    }
}
