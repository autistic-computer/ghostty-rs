//! Port of `src/terminal/kitty/dnd_test.zig`.
//!
//! STATUS: ported
//!
//! End-to-end tests for the OSC 72 protocol state machine, validating
//! wire behavior against kitty's implementation (using kitty_tests/dnd.py as
//! an oracle for the expected bytes).
//!
//! It isn't normal for us to have dedicated test files but in this case
//! the dnd protocol is complicated enough that I wanted full e2e covering
//! the full machine.
//!
//! Deviation: `h.registered().dragMove(alloc, &h.output.writer, ...)`
//! becomes `h.drag_move(...)` (and likewise for drop/leave) since the
//! state and the output collector can't both be borrowed through
//! `registered()`.
#![cfg(test)]

use crate::terminal::osc;

use super::dnd;

/// A test harness holding the lazily allocated protocol state and an
/// output collector.
struct Harness {
    state: Option<Box<dnd::State>>,
    output: Vec<u8>,
}

impl Harness {
    fn init() -> Harness {
        Harness {
            state: None,
            output: Vec::new(),
        }
    }

    /// The registered state; asserts a client has registered.
    fn registered(&mut self) -> &mut dnd::State {
        self.state.as_mut().unwrap()
    }

    /// Feed one client command, as it would arrive from the OSC parser,
    /// returning the event the stream handler would pass to its effect.
    fn command(&mut self, metadata: &[u8], payload: Option<&[u8]>) -> Option<dnd::Event> {
        dnd::handle_command(
            &mut self.state,
            &mut self.output,
            osc::KittyDndProtocol {
                metadata,
                payload,
                terminator: osc::Terminator::St,
            },
        )
        .unwrap()
    }

    fn drag_move(&mut self, ev: dnd::MoveEvent, mimes: &[&[u8]]) {
        self.state
            .as_mut()
            .unwrap()
            .drag_move(&mut self.output, ev, mimes)
            .unwrap();
    }

    fn drag_drop(&mut self, ev: dnd::MoveEvent, items: &[dnd::Item<'_>]) {
        self.state
            .as_mut()
            .unwrap()
            .drag_drop(&mut self.output, ev, items)
            .unwrap();
    }

    fn drag_leave(&mut self) {
        self.state
            .as_mut()
            .unwrap()
            .drag_leave(&mut self.output)
            .unwrap();
    }

    /// Consume and return the collected output.
    fn consume(&self) -> &[u8] {
        &self.output
    }

    fn clear(&mut self) {
        self.output.clear();
    }

    fn expect_output(&mut self, expected: &str) {
        assert_eq!(expected, String::from_utf8_lossy(&self.output));
        self.clear();
    }
}

fn copy_ev(cell_x: u32, cell_y: u32, pixel_x: i32, pixel_y: i32) -> dnd::MoveEvent {
    dnd::MoveEvent {
        cell_x,
        cell_y,
        pixel_x,
        pixel_y,
        operations: dnd::Operations {
            copy: true,
            r#move: false,
        },
    }
}

fn zero_ev() -> dnd::MoveEvent {
    copy_ev(0, 0, 0, 0)
}

#[test]
fn dnd_query_response() {
    let mut h = Harness::init();

    // Works without any registration, matching kitty, and allocates
    // nothing.
    let _ = h.command(b"t=q", None);
    h.expect_output("\x1b]72;t=q\x1b\\");
    assert!(h.state.is_none());
}

#[test]
fn dnd_query_response_echoes_client_id() {
    let mut h = Harness::init();

    let _ = h.command(b"t=q:i=31", None);
    h.expect_output("\x1b]72;t=q:i=31\x1b\\");
}

#[test]
fn dnd_register_and_unregister() {
    let mut h = Harness::init();

    assert!(h.state.is_none());

    // Registration allocates the state and reports it, with the
    // declared MIME list readable from the state.
    assert_eq!(
        Some(dnd::Event::Registration),
        h.command(b"t=a", Some(b"text/plain text/uri-list"))
    );
    h.expect_output("");
    {
        let mut it = h.registered().registered_mimes();
        assert_eq!(b"text/plain".as_slice(), it.next().unwrap());
        assert_eq!(b"text/uri-list".as_slice(), it.next().unwrap());
        assert!(it.next().is_none());
    }

    // Machine ID declaration is accepted and ignored.
    assert_eq!(None, h.command(b"t=a:x=1", Some(b"1:deadbeef")));
    h.expect_output("");
    assert!(h.state.is_some());

    // Re-registration replaces the list.
    assert_eq!(
        Some(dnd::Event::Registration),
        h.command(b"t=a", Some(b"image/png"))
    );
    {
        let mut it = h.registered().registered_mimes();
        assert_eq!(b"image/png".as_slice(), it.next().unwrap());
        assert!(it.next().is_none());
    }

    // Registering without a list is the common case.
    assert_eq!(Some(dnd::Event::Registration), h.command(b"t=a", None));
    {
        let mut it = h.registered().registered_mimes();
        assert!(it.next().is_none());
    }

    // Unregistration frees it and reports the change.
    assert_eq!(Some(dnd::Event::Registration), h.command(b"t=A", None));
    h.expect_output("");
    assert!(h.state.is_none());

    // Unregistering again changes nothing.
    assert_eq!(None, h.command(b"t=A", None));
    h.expect_output("");
    assert!(h.state.is_none());
}

#[test]
fn dnd_no_state_before_registration() {
    let mut h = Harness::init();

    // State-dependent commands from an unregistered client allocate
    // nothing; a data request gets the error kitty sends from its
    // zeroed state.
    let _ = h.command(b"t=m:o=1", Some(b"text/plain"));
    let _ = h.command(b"t=r", None);
    h.expect_output("");
    let _ = h.command(b"t=r:x=1", None);
    h.expect_output("\x1b]72;t=R:x=1:m=0;ENOENT:no drop data available\x1b\\");
    assert!(h.state.is_none());
}

#[test]
fn dnd_move_event_carries_position_operations_and_mime_list() {
    let mut h = Harness::init();

    let _ = h.command(b"t=a", Some(b"text/plain"));

    h.drag_move(copy_ev(5, 3, 100, 60), &[b"text/plain", b"text/uri-list"]);

    // Note the trailing space after every MIME entry, matching kitty.
    h.expect_output("\x1b]72;t=m:x=5:y=3:X=100:Y=60:o=1:m=0;text/plain text/uri-list \x1b\\");
}

#[test]
fn dnd_move_event_echoes_registration_client_id() {
    let mut h = Harness::init();

    let _ = h.command(b"t=a:i=7", Some(b""));
    h.drag_move(
        dnd::MoveEvent {
            cell_x: 1,
            cell_y: 2,
            pixel_x: 8,
            pixel_y: 16,
            operations: dnd::Operations {
                copy: true,
                r#move: true,
            },
        },
        &[b"text/plain"],
    );
    h.expect_output("\x1b]72;t=m:x=1:y=2:X=8:Y=16:o=3:i=7:m=0;text/plain \x1b\\");
}

#[test]
fn dnd_re_registration_updates_client_id_in_place() {
    let mut h = Harness::init();

    let _ = h.command(b"t=a:i=7", Some(b""));
    let state: *const dnd::State = h.registered();
    let _ = h.command(b"t=a:i=9", Some(b""));
    // Same allocation, new client ID.
    assert!(std::ptr::eq(&**h.state.as_ref().unwrap(), state));
    assert_eq!(9u32, h.registered().drop.client_id);
}

#[test]
fn dnd_mime_list_sent_on_every_move() {
    let mut h = Harness::init();

    let _ = h.command(b"t=a", Some(b""));
    let ev = zero_ev();
    h.drag_move(ev, &[b"text/plain"]);
    h.clear();

    // Kitty resends the list even when unchanged; clients depend on it.
    h.drag_move(ev, &[b"text/plain"]);
    h.expect_output("\x1b]72;t=m:x=0:y=0:X=0:Y=0:o=1:m=0;text/plain \x1b\\");
}

#[test]
fn dnd_leave_event() {
    let mut h = Harness::init();

    let _ = h.command(b"t=a", Some(b""));
    h.drag_move(zero_ev(), &[b"text/plain"]);
    h.clear();

    h.drag_leave();
    h.expect_output("\x1b]72;t=m:x=-1:y=-1\x1b\\");
}

#[test]
fn dnd_client_acceptance_recorded() {
    let mut h = Harness::init();

    let _ = h.command(b"t=a", Some(b""));
    assert!(h.registered().client_accepted().is_none());

    assert_eq!(
        Some(dnd::Event::Acceptance),
        h.command(b"t=m:o=1", Some(b"text/plain"))
    );
    h.expect_output("");
    assert_eq!(
        dnd::Operation::Copy,
        h.registered().client_accepted().unwrap()
    );

    // Rejection.
    assert_eq!(Some(dnd::Event::Acceptance), h.command(b"t=m:o=0", Some(b"")));
    assert_eq!(
        dnd::Operation::None,
        h.registered().client_accepted().unwrap()
    );
}

#[test]
fn dnd_chunked_client_acceptance() {
    let mut h = Harness::init();

    let _ = h.command(b"t=a", Some(b""));

    // Chunked accept: continuation metadata is ignored, the acceptance
    // is pending until the final chunk.
    assert_eq!(None, h.command(b"t=m:o=2:m=1", Some(b"text/pl")));
    assert!(h.registered().client_accepted().is_none());
    assert_eq!(None, h.command(b"t=m:m=1", Some(b"ain text")));
    assert_eq!(
        Some(dnd::Event::Acceptance),
        h.command(b"t=m:m=0", Some(b"/html"))
    );
    assert_eq!(
        dnd::Operation::Move,
        h.registered().client_accepted().unwrap()
    );

    // The accumulated list was converted to NUL-separated entries.
    assert_eq!(
        b"text/plain\x00text/html\x00".as_slice(),
        h.registered().drop.accepted_mimes.as_slice()
    );
}

#[test]
fn dnd_drop_and_data_serving_round_trip() {
    let mut h = Harness::init();

    let _ = h.command(b"t=a", Some(b"text/plain text/uri-list"));

    let ev = copy_ev(4, 2, 40, 20);
    h.drag_drop(
        ev,
        &[
            dnd::Item {
                mime: b"text/uri-list",
                data: b"file:///tmp/a.txt\r\n",
            },
            dnd::Item {
                mime: b"text/plain",
                data: b"hello",
            },
        ],
    );
    h.expect_output("\x1b]72;t=M:x=4:y=2:X=40:Y=20:o=1:m=0;text/uri-list text/plain \x1b\\");

    // Request the second MIME's data: base64 chunk plus the empty
    // end-of-data message.
    let _ = h.command(b"t=r:x=2", None);
    h.expect_output(concat!(
        "\x1b]72;t=r:x=2:m=0;aGVsbG8=\x1b\\",
        "\x1b]72;t=r:x=2\x1b\\"
    ));

    // Out-of-bounds request.
    let _ = h.command(b"t=r:x=3", None);
    h.expect_output("\x1b]72;t=R:x=3:m=0;ENOENT:drop data request index out of bounds\x1b\\");

    // Conclude: the performed operation is reported, held data is
    // freed, and further requests fail.
    assert_eq!(
        dnd::Event::ConcludedCopy,
        h.command(b"t=r:o=1", None).unwrap()
    );
    h.expect_output("");
    assert_eq!(None, h.command(b"t=r:o=1", None));
    let _ = h.command(b"t=r:x=1", None);
    h.expect_output("\x1b]72;t=R:x=1:m=0;ENOENT:no drop data available\x1b\\");
}

#[test]
fn dnd_empty_item_served_as_a_single_end_of_data_message() {
    let mut h = Harness::init();

    let _ = h.command(b"t=a", Some(b""));
    h.drag_drop(
        zero_ev(),
        &[dnd::Item {
            mime: b"text/plain",
            data: b"",
        }],
    );
    h.clear();

    // Kitty's oracle (test_empty_data) asserts exactly one message:
    // the empty response is itself the end-of-data signal, and a
    // duplicate would be a second completion to the client.
    let _ = h.command(b"t=r:x=1", None);
    h.expect_output("\x1b]72;t=r:x=1\x1b\\");
}

#[test]
fn dnd_leave_without_hover_sends_nothing() {
    let mut h = Harness::init();

    // Client registered but no move was ever forwarded (e.g. it
    // registered mid-drag): kitty only notifies hovered windows.
    let _ = h.command(b"t=a", Some(b""));
    h.drag_leave();
    h.expect_output("");
}

#[test]
fn dnd_data_request_with_no_drop() {
    let mut h = Harness::init();

    let _ = h.command(b"t=a", Some(b""));
    let _ = h.command(b"t=r:x=1", None);
    h.expect_output("\x1b]72;t=R:x=1:m=0;ENOENT:no drop data available\x1b\\");
}

#[test]
fn dnd_leave_after_drop_is_ignored() {
    let mut h = Harness::init();

    let _ = h.command(b"t=a", Some(b""));
    h.drag_drop(
        zero_ev(),
        &[dnd::Item {
            mime: b"text/plain",
            data: b"x",
        }],
    );
    h.clear();

    // Some toolkits emit a leave for the drop itself; the held data
    // must survive so the client can still fetch it.
    h.drag_leave();
    h.expect_output("");

    let _ = h.command(b"t=r:x=1", None);
    h.expect_output(concat!(
        "\x1b]72;t=r:x=1:m=0;eA==\x1b\\",
        "\x1b]72;t=r:x=1\x1b\\"
    ));
}

#[test]
fn dnd_new_drag_resets_held_drop_data() {
    let mut h = Harness::init();

    let _ = h.command(b"t=a", Some(b""));
    let ev = zero_ev();
    h.drag_drop(
        ev,
        &[dnd::Item {
            mime: b"text/plain",
            data: b"old",
        }],
    );
    h.clear();

    // A new drag entering resets the per-drag state including the held
    // items from the unconcluded previous drop.
    h.drag_move(ev, &[b"text/plain"]);
    h.clear();
    let _ = h.command(b"t=r:x=1", None);
    h.expect_output("\x1b]72;t=R:x=1:m=0;ENOENT:no drop data available\x1b\\");
}

#[test]
fn dnd_remote_transfer_requests_refused() {
    let mut h = Harness::init();

    let _ = h.command(b"t=a", Some(b""));

    // URI file content request.
    let _ = h.command(b"t=r:x=1:y=2", None);
    h.expect_output("\x1b]72;t=R:x=1:y=2:m=0;EINVAL:remote drop data is not supported\x1b\\");

    // Directory handle request.
    let _ = h.command(b"t=r:Y=2:x=1", None);
    h.expect_output("\x1b]72;t=R:x=1:Y=2:m=0;EINVAL:remote drop data is not supported\x1b\\");
}

#[test]
fn dnd_drag_out_refused() {
    let mut h = Harness::init();

    // Enabling and disabling offers is accepted silently and allocates
    // nothing.
    let _ = h.command(b"t=o:x=1", None);
    let _ = h.command(b"t=o:x=2", None);
    h.expect_output("");
    assert!(h.state.is_none());

    // Offering a drag is refused.
    let _ = h.command(b"t=o:x=1", None);
    let _ = h.command(b"t=o:o=3", Some(b"text/plain"));
    h.expect_output("\x1b]72;t=E:m=0;EPERM:drag out is not supported by this terminal\x1b\\");

    // Starting a drag is refused, echoing the command's client id.
    let _ = h.command(b"t=P:x=-1:i=9", None);
    h.expect_output("\x1b]72;t=E:i=9:m=0;EPERM:drag out is not supported by this terminal\x1b\\");
    assert!(h.state.is_none());
}

#[test]
fn dnd_unregister_frees_held_drop_data() {
    let mut h = Harness::init();

    let _ = h.command(b"t=a", Some(b""));
    h.drag_drop(
        zero_ev(),
        &[dnd::Item {
            mime: b"text/plain",
            data: b"x",
        }],
    );
    h.clear();

    // The Zig testing allocator would report the held data as leaked if
    // unregistration didn't free the whole state.
    let _ = h.command(b"t=A", None);
    assert!(h.state.is_none());
}

#[test]
fn dnd_chunked_registration_reuses_first_chunk_metadata() {
    let mut h = Harness::init();

    // Registration split over two chunks: the first chunk allocates
    // the state and seeds chunk reassembly, so the continuation (which
    // carries a different type) is still treated as the registration.
    assert_eq!(None, h.command(b"t=a:i=4:m=1", Some(b"text/pla")));
    assert!(h.state.is_some());
    assert_eq!(
        Some(dnd::Event::Registration),
        h.command(b"t=q:m=0", Some(b"in"))
    );
    h.expect_output("");
    assert_eq!(4u32, h.registered().drop.client_id);
    {
        let mut it = h.registered().registered_mimes();
        assert_eq!(b"text/plain".as_slice(), it.next().unwrap());
    }

    // A query after the chunked command completes works again.
    let _ = h.command(b"t=q", None);
    h.expect_output("\x1b]72;t=q\x1b\\");
}

#[test]
fn dnd_malformed_metadata_ignored() {
    let mut h = Harness::init();

    let _ = h.command(b"t=a:zz=1", Some(b""));
    h.expect_output("");
    assert!(h.state.is_none());

    // Command with no type is ignored, matching kitty (the spec's
    // default of t=a is not honored by the reference implementation).
    let _ = h.command(b"x=1", Some(b""));
    h.expect_output("");
    assert!(h.state.is_none());
}

#[test]
fn dnd_bel_terminator_echoed_in_responses() {
    let mut h = Harness::init();

    let _ = dnd::handle_command(
        &mut h.state,
        &mut h.output,
        osc::KittyDndProtocol {
            metadata: b"t=q",
            payload: None,
            terminator: osc::Terminator::Bel,
        },
    )
    .unwrap();
    h.expect_output("\x1b]72;t=q\x07");
}

#[test]
fn dnd_kitten_0_47_conversation_replay() {
    // This replays a conversation recorded from the reference client
    // (`kitten dnd --drop-anywhere=copy --drop text/plain:out.txt`,
    // kitten 0.47.0) driven over a pty by a harness that sent exactly
    // the bytes this engine produces. The kitten accepted the events,
    // wrote the dropped payload to disk intact, and concluded; its
    // client bytes are frozen here as an interop regression test.
    let mut h = Harness::init();

    // Startup: register with MIME list and machine ID, then the test
    // harness reset (unregister both directions, re-register).
    let _ = h.command(b"t=a:m=0", Some(b"text/uri-list text/plain"));
    let _ = h.command(
        b"t=a:x=1:m=0",
        Some(b"1:5cff8247c477900a8727e2281fe890252f8848f87c224dd8dd7fb6303e94ddbd"),
    );
    let _ = h.command(b"t=A", None);
    assert!(h.state.is_none());
    let _ = h.command(b"t=o:x=2", None);
    let _ = h.command(b"t=a:m=0", Some(b"text/uri-list text/plain"));
    let _ = h.command(
        b"t=a:x=1:m=0",
        Some(b"1:5cff8247c477900a8727e2281fe890252f8848f87c224dd8dd7fb6303e94ddbd"),
    );
    h.expect_output("");
    assert!(h.state.is_some());

    // Native drag moves over the terminal and drops.
    let ev = copy_ev(2, 1, 20, 18);
    h.drag_move(ev, &[b"text/plain"]);
    h.expect_output("\x1b]72;t=m:x=2:y=1:X=20:Y=18:o=1:m=0;text/plain \x1b\\");

    // The kitten accepts as a copy of text/plain.
    let _ = h.command(b"t=m:o=1:m=0", Some(b"text/plain"));
    h.expect_output("");
    assert_eq!(
        dnd::Operation::Copy,
        h.registered().client_accepted().unwrap()
    );

    h.drag_drop(
        ev,
        &[dnd::Item {
            mime: b"text/plain",
            data: b"hello from ghostty\n",
        }],
    );
    h.expect_output("\x1b]72;t=M:x=2:y=1:X=20:Y=18:o=1:m=0;text/plain \x1b\\");

    // The kitten requests the data and concludes with a copy.
    let _ = h.command(b"t=r:x=1", None);
    h.expect_output(concat!(
        "\x1b]72;t=r:x=1:m=0;aGVsbG8gZnJvbSBnaG9zdHR5Cg==\x1b\\",
        "\x1b]72;t=r:x=1\x1b\\"
    ));
    let _ = h.command(b"t=r:o=1", None);
    h.expect_output("");
    assert!(h.registered().drop.items.is_none());
}

#[test]
fn dnd_large_data_served_in_chunks() {
    let mut h = Harness::init();

    let _ = h.command(b"t=a", Some(b""));

    // 3073 bytes: one full chunk plus one byte.
    let data = [b'Z'; 3073];
    h.drag_drop(
        zero_ev(),
        &[dnd::Item {
            mime: b"application/octet-stream",
            data: &data,
        }],
    );
    h.clear();

    let _ = h.command(b"t=r:x=1", None);
    let out = h.consume();

    // First chunk is m=1 with 4096 base64 chars, second is m=0, and
    // the final message is the bare end-of-data marker.
    assert!(out.starts_with(b"\x1b]72;t=r:x=1:m=1;"));
    assert!(out
        .windows(b"\x1b]72;t=r:x=1:m=0;".len())
        .any(|w| w == b"\x1b]72;t=r:x=1:m=0;"));
    assert!(out.ends_with(b"\x1b]72;t=r:x=1\x1b\\"));
    h.clear();
}

#[test]
fn dnd_over_cap_registration_list_never_completes() {
    let mut h = Harness::init();

    // Matching kitty, a chunk that would exceed the cap is dropped and
    // the registration is not reported, though the client stays
    // registered (the state exists).
    let big = vec![b'a'; dnd::MAX_MIME_LIST_BYTES + 1];
    assert_eq!(None, h.command(b"t=a", Some(&big)));
    assert!(h.state.is_some());
    {
        let mut it = h.registered().registered_mimes();
        assert!(it.next().is_none());
    }
}
