//! Port of `src/terminal/kitty/graphics_exec.zig`.
//!
//! STATUS: ported
//!
//! Notes on the port:
//! - `std.Io`/allocator parameters are dropped.
//! - The storage lives in the active screen, so the functions below borrow
//!   the screen's `kitty_images` and `pages` separately and pass the
//!   terminal values the storage reads as a [`TerminalInfo`].
//! - `Terminal.kittyGraphics` (a wrapper of `execute`) is defined here, as
//!   the `Terminal` port expects.

use crate::terminal::kitty::graphics_animation as animation;
use crate::terminal::kitty::graphics_command::{self as command, Command, Control, Response};
use crate::terminal::kitty::graphics_image::{self as image, Data, Image, LoadingImage};
use crate::terminal::kitty::graphics_pixel as pixel;
use crate::terminal::kitty::graphics_storage::{
    Location, NextImageIdMode, ParentError, Placement, PlacementId, PlacementKey, PlacementTag,
    Relative, TerminalInfo,
};
use crate::terminal::screen::Screen;
use crate::terminal::size::AllocError;
use crate::terminal::terminal::Terminal;

const LOG_TARGET: &str = "kitty_gfx";

/// The [`TerminalInfo`] of a terminal (its geometry, scrolling region and
/// the active screen's cursor).
pub fn terminal_info(t: &Terminal) -> TerminalInfo {
    TerminalInfo::from_terminal(t)
}

impl Terminal {
    /// Execute a kitty graphics command against this terminal (Zig:
    /// `Terminal.kittyGraphics`).
    pub fn kitty_graphics(&mut self, cmd: &Command) -> Option<Response> {
        execute(self, cmd)
    }
}

/// Execute a Kitty graphics command against the given terminal. This
/// will never fail, but the response may indicate an error and the
/// terminal state may not be updated to reflect the command. This will
/// never put the terminal in an unrecoverable state, however.
pub fn execute(terminal: &mut Terminal, cmd: &Command) -> Option<Response> {
    // If storage is disabled then we disable the full protocol. This means
    // we don't even respond to queries so the terminal completely acts as
    // if this feature is not supported.
    if !terminal.screens.active.kitty_images.enabled() {
        log::debug!(target: LOG_TARGET, "kitty graphics requested but disabled");
        return None;
    }

    log::debug!(
        target: LOG_TARGET,
        "executing kitty graphics command: quiet={:?} control={:?}",
        cmd.quiet,
        cmd.control
    );

    // The quiet settings used to control the response. We have to make this
    // a var because in certain special cases (namely chunked transmissions)
    // this can change.
    let mut quiet = cmd.quiet;

    // The protocol makes i and I mutually exclusive for every action, so this
    // must happen before dispatch and before an action can mutate storage.
    // https://sw.kovidgoyal.net/kitty/graphics-protocol/#requesting-image-ids-from-the-terminal
    let identifiers = cmd.control.identifiers();
    if identifiers.image_id > 0 && identifiers.image_number > 0 {
        let resp = Response {
            id: identifiers.image_id,
            image_number: identifiers.image_number,
            placement_id: identifiers.placement_id,
            message: "EINVAL: image ID and number are mutually exclusive",
            ..Default::default()
        };
        log::warn!(target: LOG_TARGET, "erroneous kitty graphics response: {}", resp.message);

        return match quiet {
            command::Quiet::No => Some(resp),
            command::Quiet::Ok => Some(resp),
            command::Quiet::Failures => None,
        };
    }

    let resp_: Option<Response> = match cmd.control {
        Control::Query(_) => Some(query(terminal, cmd)),
        Control::Display(_) => Some(display(terminal, cmd)),
        Control::Delete(_) => Some(delete(terminal, cmd)),

        Control::Transmit(_) | Control::TransmitAndDisplay(_) | Control::TransmitAnimationFrame(_) => {
            let storage = &mut terminal.screens.active.kitty_images;

            // Only direct (t=d) data can be sent in chunks, so only a
            // direct command can continue a chunked load. A file, temporary
            // file, or shared memory command abandons the load and is
            // handled as a brand new transmission. Kitty does the same for
            // both image and animation frame loads.
            if storage.loading.is_some()
                && cmd.transmission().unwrap().medium != command::Medium::Direct
            {
                storage.loading = None;
            }

            // If we're transmitting, then our `q` setting value is complicated.
            // The `q` setting inherits the value from the starting command
            // unless `q` is set >= 1 on this command. If it is, then we save
            // that as the new `q` setting.
            if let Some(loading) = storage.loading.as_mut() {
                match cmd.quiet {
                    // q=0 we use whatever the start command value is
                    command::Quiet::No => quiet = loading.quiet,

                    // q>=1 we use the new value, but we should already be set to it
                    tag @ (command::Quiet::Ok | command::Quiet::Failures) => {
                        debug_assert!(quiet == tag);
                        loading.quiet = tag;
                    }
                }
            }

            Some(match cmd.control {
                Control::TransmitAnimationFrame(_) => transmit_animation_frame(terminal, cmd),
                _ => transmit(terminal, cmd),
            })
        }

        Control::ControlAnimation(_) => Some(control_animation(terminal, cmd)),

        Control::ComposeAnimation(_) => Some(compose_animation(terminal, cmd)),
    };

    // Handle the quiet settings
    if let Some(resp) = resp_ {
        if !resp.ok() {
            log::warn!(target: LOG_TARGET, "erroneous kitty graphics response: {}", resp.message);
        }

        return match quiet {
            command::Quiet::No => {
                if resp.empty() {
                    None
                } else {
                    Some(resp)
                }
            }
            command::Quiet::Ok => {
                if resp.ok() {
                    None
                } else {
                    Some(resp)
                }
            }
            command::Quiet::Failures => None,
        };
    }

    None
}

/// Execute a "query" command.
///
/// This command is used to attempt to load an image and respond with
/// success/error but does not persist any of the command to the terminal
/// state.
fn query(terminal: &Terminal, cmd: &Command) -> Response {
    let Control::Query(t) = cmd.control else {
        unreachable!()
    };

    // Query requires image ID. We can't actually send a response without
    // an image ID either but we return an error and this will be logged
    // downstream.
    if t.image_id == 0 {
        return Response {
            message: "EINVAL: image ID required",
            ..Default::default()
        };
    }

    // Build a partial response to start
    let mut result = Response {
        id: t.image_id,
        image_number: t.image_number,
        placement_id: t.placement_id,
        ..Default::default()
    };

    // A query must attempt a complete load, then discard the result without
    // changing image storage.
    // https://sw.kovidgoyal.net/kitty/graphics-protocol/#querying-support-and-available-transmission-mediums
    let storage = &terminal.screens.active.kitty_images;
    let mut loading = match LoadingImage::init(cmd, &storage.image_limits) {
        Ok(l) => l,
        Err(err) => {
            encode_error(&mut result, err);
            return result;
        }
    };

    if let Err(err) = loading.complete() {
        encode_error(&mut result, err);
        return result;
    }

    result
}

/// Transmit image data.
///
/// This loads the image, validates it, and puts it into the terminal
/// screen storage. It does not display the image.
fn transmit(terminal: &mut Terminal, cmd: &Command) -> Response {
    let t = cmd.transmission().unwrap();
    let storage = &terminal.screens.active.kitty_images;

    let mut result: Response = if let Some(loading) = &storage.loading {
        // A load is in progress, so this command is a continuation chunk.
        // execute has already abandoned the load if this command used a
        // medium other than direct, so we know this chunk belongs here.
        // The load completes according to how it started. If an animation
        // frame load (a=f) is in progress, this chunk belongs to it even
        // when the client didn't repeat a=f on the chunk.
        if loading.frame.is_some() {
            return transmit_animation_frame(terminal, cmd);
        }

        loading.response
    } else {
        Response {
            id: t.image_id,
            image_number: t.image_number,
            placement_id: t.placement_id,
            ..Default::default()
        }
    };

    let load = match load_and_add_image(terminal, cmd) {
        Ok(l) => l,
        Err(err) => {
            encode_error(&mut result, err);
            return result;
        }
    };

    // If we're also displaying, then do that now. This function does
    // both transmit and transmit and display. The display might also be
    // deferred if it is multi-chunk.
    if let Some(d) = load.display {
        debug_assert!(!load.more);
        let mut d_copy = d;
        d_copy.image_id = load.image_id;
        result = display(
            terminal,
            &Command {
                control: Control::Display(d_copy),
                quiet: cmd.quiet,
                data: Vec::new(),
            },
        );
    }

    // If there are more chunks expected we do not respond.
    if load.more {
        return Response::default();
    }

    // If the loaded image was assigned its ID automatically, not based
    // on a number or explicitly specified ID, then we don't respond.
    if load.implicit_id {
        return Response::default();
    }

    // After the image is added, set the ID in case it changed.
    // The resulting image number and placement ID never change.
    result.id = load.image_id;

    result
}

/// Display a previously transmitted image.
fn display(terminal: &mut Terminal, cmd: &Command) -> Response {
    let d = cmd.display().unwrap();

    // Display requires image ID or number.
    if d.image_id == 0 && d.image_number == 0 {
        return Response {
            message: "EINVAL: image ID or number required",
            ..Default::default()
        };
    }

    // Build up our response
    let mut result = Response {
        id: d.image_id,
        image_number: d.image_number,
        placement_id: d.placement_id,
        ..Default::default()
    };

    // A virtual placement (U=1) cannot also be a relative placement.
    // Kitty checks this before even looking up the image.
    if d.virtual_placement && d.parent_id > 0 {
        result.message = "EINVAL: virtual placement cannot refer to a parent";
        return result;
    }

    let info = terminal_info(terminal);
    let screen: &mut Screen = &mut terminal.screens.active;
    let storage = &mut screen.kitty_images;

    // Verify the requested image exists if we have an ID
    let img_ = if d.image_id != 0 {
        storage.image_by_id(d.image_id)
    } else {
        storage.image_by_number(d.image_number)
    };
    let Some(img) = img_ else {
        result.message = "ENOENT: image not found";
        return result;
    };
    // A metadata copy (Zig copies the `Image` value).
    let img = img.without_data();

    // Make sure our response has the image id in case we looked up by number
    result.id = img.id;

    // Location where the placement will go.
    let location: Location = if d.virtual_placement {
        // Virtual placements are not tracked
        Location::Virtual
    } else if d.parent_id == 0 {
        // No parent reference (P=): the placement is pinned to the
        // cursor. The cursor is always tracked but we don't want
        // this pin to move with the cursor.
        // SAFETY: the cursor pin is tracked by the screen's page list.
        let cursor_pin = unsafe { *screen.cursor.page_pin.as_ptr() };
        match screen.pages.track_pin(cursor_pin) {
            Ok(pin) => Location::Pin(pin),
            Err(err) => {
                log::warn!(target: LOG_TARGET, "failed to create pin for Kitty graphics err={err:?}");
                result.message = "EINVAL: failed to prepare terminal state";
                return result;
            }
        }
    } else {
        // A parent reference makes this a relative placement: it is
        // positioned relative to the parent placement instead of the
        // cursor.

        // The key of the placement being created, when it is
        // addressable (an explicit placement ID). Needed for
        // self-parent and cycle detection.
        let child: Option<PlacementKey> = if d.placement_id > 0 {
            Some(PlacementKey {
                image_id: img.id,
                placement_id: PlacementId {
                    tag: PlacementTag::External,
                    id: d.placement_id,
                },
            })
        } else {
            None
        };

        let parent = match screen.kitty_images.resolve_parent(
            &mut screen.pages,
            child,
            d.parent_id,
            d.parent_placement_id,
        ) {
            Ok(p) => p,
            Err(err) => {
                result.message = match err {
                    ParentError::ParentImageNotFound => "ENOPARENT: parent image not found",
                    ParentError::ParentPlacementNotFound => "ENOPARENT: parent placement not found",
                    ParentError::SelfParent => "EINVAL: placement cannot be its own parent",
                    ParentError::Cycle => "ECYCLE: parent chain creates a cycle",
                    ParentError::TooDeep => "ETOODEEP: parent chain too deep",
                    ParentError::AncestorNotFound => "ENOENT: parent chain ancestor not found",
                };
                return result;
            }
        };

        Location::Relative(Relative {
            parent,
            horizontal_offset: d.horizontal_offset,
            vertical_offset: d.vertical_offset,
        })
    };

    // Add the placement
    let p: Placement = {
        let mut p = Placement {
            location,
            x_offset: d.x_offset,
            y_offset: d.y_offset,
            source_x: d.x,
            source_y: d.y,
            source_width: d.width,
            source_height: d.height,
            columns: d.columns,
            rows: d.rows,
            z: d.z,
        };

        let cell_offset = p.cell_offset(&info);
        if info.width_px / info.cols as u32 > 0 {
            p.x_offset = cell_offset.x;
        }
        if info.height_px / info.rows as u32 > 0 {
            p.y_offset = cell_offset.y;
        }

        p
    };
    if let Err(err) = screen
        .kitty_images
        .add_placement(&mut screen.pages, img.id, result.placement_id, p)
    {
        p.deinit(&mut screen.pages);
        encode_error(&mut result, alloc_error(err));
        return result;
    }

    // Apply cursor movement setting. This only applies to pin placements:
    // relative placements never move the cursor regardless of C=, just
    // like kitty.
    match p.location {
        Location::Virtual | Location::Relative(_) => {}
        Location::Pin(pin) => match d.cursor_movement {
            command::CursorMovement::None => {}
            command::CursorMovement::After => {
                // We use terminal.index to properly handle scroll regions.
                let size = p.grid_size(&img, &info);
                // SAFETY: the pin is tracked by the screen's page list.
                let pin_x = unsafe { pin.as_ref().x };
                let target_x = (pin_x as usize).saturating_add(size.cols as usize);
                let wraps = target_x >= info.cols as usize;
                let requested_rows = (size.rows as usize)
                    .saturating_sub(1)
                    .saturating_add(wraps as usize);

                // The requested row count comes from the application and can
                // be as large as u32. Calling terminal.index once for every
                // row could therefore leave the terminal unresponsive for a
                // very long time.
                //
                // First allow enough calls to reach the bottom of the scroll
                // region. Each call after that scrolls the region by one row,
                // so limit those extra calls to one screen. This follows
                // Kitty's behavior while keeping the amount of work bounded.
                let region = info.scrolling_region;
                let cursor = &screen.cursor;
                let rows_before_scroll: usize = if cursor.y >= region.top
                    && cursor.y <= region.bottom
                    && cursor.x >= region.left
                    && cursor.x <= region.right
                {
                    (region.bottom - cursor.y) as usize
                } else {
                    0
                };
                let rows_to_move: usize =
                    requested_rows.min(rows_before_scroll.saturating_add(info.rows as usize));
                for _ in 0..rows_to_move {
                    if let Err(err) = terminal.index() {
                        log::warn!(target: LOG_TARGET, "failed to move cursor: {err:?}");
                        break;
                    }
                }

                // Kitty wraps once when the movement reaches the right edge;
                // otherwise it leaves the cursor immediately to the right of
                // the placement.
                let screen: &mut Screen = &mut terminal.screens.active;
                screen.cursor.pending_wrap = false;
                screen.cursor_horizontal_absolute(if wraps { 0 } else { target_x as _ });
            }
        },
    }

    result
}

/// Transmit animation frame data (a=f).
///
/// Frame data is loaded exactly like image data, including chunking
/// and every transmission medium, and once complete it is composed
/// into a frame of an existing image's animation.
fn transmit_animation_frame(terminal: &mut Terminal, cmd: &Command) -> Response {
    let storage = &mut terminal.screens.active.kitty_images;

    // A chunk arriving while a load is in progress continues that load.
    if let Some(loading) = storage.loading.as_mut() {
        // If this is the first frame, then we treat it like a normal image
        // because the first frame is just the image.
        if loading.frame.is_none() {
            return transmit(terminal, cmd);
        }

        // Not the first frame, so continue loading similar to transmit
        // but this is for a subsequent frame.
        let mut result = loading.response;
        if let Err(err) = loading.add_data(&cmd.data) {
            encode_error(&mut result, err);
            return result;
        }

        // If more chunks are expected we don't respond yet.
        let t = cmd.transmission().unwrap();
        if t.more_chunks {
            return Response::default();
        }

        // Final chunk: take the loading state out, mirroring
        // loadAndAddImage.
        let mut loading = storage.loading.take().unwrap();
        return complete_animation_frame(terminal, &mut loading);
    }

    // We are starting our load, either the first frame or a subsequent frame.
    let Control::TransmitAnimationFrame(f) = cmd.control else {
        unreachable!()
    };
    let t = f.transmission;
    let mut result = Response {
        id: t.image_id,
        image_number: t.image_number,
        placement_id: t.placement_id,
        // Errors echo the client's frame number; on success this is
        // replaced with the resolved (possibly newly assigned) one.
        frame: f.edit_frame,
        ..Default::default()
    };

    // A frame can only be added to an existing image.
    if t.image_id == 0 && t.image_number == 0 {
        result.message = "EINVAL: image ID or number required";
        return result;
    }
    let Some(img) = storage.image_ptr_by_id_or_number(t.image_id, t.image_number) else {
        result.message = "ENOENT: image not found";
        return result;
    };
    result.id = img.id;
    let img_id = img.id;
    let img_generation = img.generation;

    let mut loading = match LoadingImage::init(cmd, &storage.image_limits) {
        Ok(l) => l,
        Err(err) => {
            encode_error(&mut result, err);
            return result;
        }
    };
    loading.frame = Some(image::FrameContext {
        cmd: f,
        image_generation: img_generation,
    });
    loading.response.id = img_id;

    // Chunked: store the loading state and wait for the rest. The
    // frame parameters above are saved with it; continuation chunks
    // contribute only payload bytes.
    if t.more_chunks {
        storage.loading = Some(Box::new(loading));
        return Response::default();
    }

    complete_animation_frame(terminal, &mut loading)
}

/// Complete a fully loaded animation frame: validate it against its
/// image and compose it into the image's animation.
fn complete_animation_frame(terminal: &mut Terminal, loading: &mut LoadingImage) -> Response {
    let screen: &mut Screen = &mut terminal.screens.active;
    let storage = &mut screen.kitty_images;
    let mut result = loading.response;
    let frame_ctx = loading.frame.unwrap();
    let f = frame_ctx.cmd;

    // Re-resolve the image: it may have been deleted, evicted, or
    // replaced while the frame data was being transmitted. The saved
    // generation pins the exact image the load started against.
    let Some(img) = storage.image_ptr_by_id_or_number(loading.image.id, loading.image.number) else {
        result.message = "ENOENT: image not found";
        return result;
    };
    if img.generation != frame_ctx.image_generation {
        result.message = "ENOENT: image not found";
        return result;
    }
    if img.data.bytes().is_none() {
        result.message = "EINVAL: image data incomplete";
        return result;
    }
    let image_id = img.id;
    let img_width = img.width;
    let img_height = img.height;

    // Finish decoding the frame data: decompression, PNG decoding,
    // and length validation all match image loading.
    let mut frame_img = match loading.complete() {
        Ok(i) => i,
        Err(err) => {
            encode_error(&mut result, err);
            return result;
        }
    };

    // The frame rectangle may not exceed the image's size. The x/y
    // offsets are deliberately not validated: composition clips,
    // matching Kitty.
    if frame_img.width > img_width || frame_img.height > img_height {
        result.message = "EINVAL: frame dimensions exceed image";
        return result;
    }

    // All composition happens in RGBA; convert both sides as needed.
    if frame_img.format != command::Format::Rgba {
        let rgba = pixel::rgba_from_format(frame_img.format, frame_img.data.bytes().unwrap());
        frame_img.data = Data::Complete(rgba);
        frame_img.format = command::Format::Rgba;
    }
    if let Err(err) = storage.convert_image_to_rgba(image_id) {
        encode_error(&mut result, alloc_error(err));
        return result;
    }

    let img = storage.images.get_mut(&image_id).unwrap();
    let anim = ensure_animation(img);

    // Resolve the frame number: r in 1..count edits that existing
    // frame, anything else (including omitted) creates a new frame
    // appended at count+1. The resolved number is echoed in the
    // response so clients can learn assigned frame numbers.
    let count: u32 = anim.frame_count();
    let number: u32 = {
        let r = f.edit_frame;
        if r == 0 || r > count + 1 {
            count + 1
        } else {
            r
        }
    };
    result.frame = number;

    let src = frame_img.data.bytes().unwrap();
    if number == count + 1 {
        // Creating a new frame. The gap defaults to 40ms when omitted
        // and a negative gap creates a gapless (never shown) frame.
        let gap: u32 = if f.gap_ms > 0 {
            f.gap_ms as u32
        } else if f.gap_ms < 0 {
            0
        } else {
            animation::DEFAULT_GAP_MS
        };

        // The base canvas frame must exist before we reserve space.
        if f.create_frame > 0 && img.frame_data(f.create_frame).is_none() {
            result.message = "EINVAL: base frame not found";
            return result;
        }

        // Reserve room for the new frame, evicting other images if
        // needed. Eviction can remove images (never this one), so we
        // re-resolve our pointer afterwards to be safe.
        let frame_len: usize = img_width as usize * img_height as usize * 4;
        if storage
            .reserve_animation_bytes(&mut screen.pages, image_id, frame_len)
            .is_err()
        {
            result.message = "ENOSPC: animation frame storage full";
            return result;
        }
        let img = storage.image_ptr_by_id_or_number(image_id, 0).unwrap();

        let mut canvas: Vec<u8> = Vec::new();
        if canvas.try_reserve_exact(frame_len).is_err() {
            storage.release_animation_bytes(frame_len);
            result.message = "ENOMEM: out of memory";
            return result;
        }
        canvas.resize(frame_len, 0);
        if f.create_frame > 0 {
            canvas.copy_from_slice(img.frame_data(f.create_frame).unwrap());
        } else {
            pixel::fill_background(&mut canvas, f.background);
        }
        pixel::compose_rect(
            &mut canvas,
            img_width,
            img_height,
            src,
            frame_img.width,
            frame_img.height,
            f.x,
            f.y,
            f.composition_mode,
        );

        let anim = img.animation.as_mut().unwrap();
        if anim.frames.try_reserve(1).is_err() {
            storage.release_animation_bytes(frame_len);
            result.message = "ENOMEM: out of memory";
            return result;
        }
        anim.frames.push(animation::Frame {
            data: canvas,
            gap_ms: gap,
            pending_len: 0,
        });

        // A new frame never changes the displayed frame (playback
        // reaches it later), but the storage content changed.
        storage.mark_mutated();
    } else {
        // Editing an existing frame. A nonzero gap also updates the
        // frame's gap; the 40ms default doesn't apply to edits.
        //
        // Unlike frame creation there is no byte reservation here:
        // frames are always stored at full image size, so the edit
        // composes into the existing buffer in place and storage
        // usage cannot change. Kitty likewise exempts frame edits
        // from its quota check.
        //
        // A frame whose pixels have not arrived yet, such as one
        // restored from a snapshot, has no buffer to draw into. The
        // edit is refused before anything about the frame changes.
        if img.frame_data(number).is_none() {
            result.message = "EINVAL: frame data incomplete";
            return result;
        }
        let anim = img.animation.as_mut().unwrap();
        if f.gap_ms != 0 {
            anim.set_gap_at(number - 1, if f.gap_ms > 0 { f.gap_ms as u32 } else { 0 });
        }
        let current = number - 1 == anim.current_index;

        // The frame data is owned by this storage (Zig: the const cast,
        // same reasoning as the renderer's image uploads).
        let dst = img.frame_data_mut(number).unwrap();
        pixel::compose_rect(
            dst,
            img_width,
            img_height,
            src,
            frame_img.width,
            frame_img.height,
            f.x,
            f.y,
            f.composition_mode,
        );

        if current {
            // The displayed pixels changed; restart the frame's gap
            // timer like Kitty's re-upload does.
            img.animation.as_mut().unwrap().frame_shown_at_ms = None;
            storage.mark_image_content_changed(image_id);
        } else {
            storage.mark_mutated();
        }
    }

    result
}

/// Control animation playback (a=a).
///
/// Successful commands never respond. The only possible responses are
/// for a missing image or identifier. Invalid key values are silently
/// ignored, matching Kitty.
fn control_animation(terminal: &mut Terminal, cmd: &Command) -> Response {
    let Control::ControlAnimation(a) = cmd.control else {
        unreachable!()
    };
    let storage = &mut terminal.screens.active.kitty_images;

    let mut result = Response {
        id: a.image_id,
        image_number: a.image_number,
        placement_id: a.placement_id,
        ..Default::default()
    };
    if a.image_id == 0 && a.image_number == 0 {
        result.message = "EINVAL: image ID or number required";
        return result;
    }
    let Some(img) = storage.image_ptr_by_id_or_number(a.image_id, a.image_number) else {
        result.message = "ENOENT: image not found";
        return result;
    };
    let image_id = img.id;

    let anim = ensure_animation(img);

    // The keys below are applied independently and in the same order as Kitty.
    let mut mutated = false;
    let mut content_changed = false;

    // Set a frame's gap (r= together with z=). This is the only way
    // to give the root frame a gap, since it is created gapless.
    if a.frame != 0 && a.frame <= anim.frame_count() && a.gap_ms != 0 {
        anim.set_gap_at(a.frame - 1, if a.gap_ms > 0 { a.gap_ms as u32 } else { 0 });
        mutated = true;
    }

    // Set the current frame (c=), the client-driven animation
    // primitive.
    if a.current_frame != 0
        && a.current_frame <= anim.frame_count()
        && a.current_frame - 1 != anim.current_index
    {
        anim.current_index = a.current_frame - 1;
        anim.frame_shown_at_ms = None;
        content_changed = true;
    }

    // Set the playback state (s=). Any state change resets the loop
    // counter; leaving the stopped state restarts the gap timer.
    if a.action != command::AnimationAction::Invalid {
        let old = anim.state;
        anim.state = match a.action {
            command::AnimationAction::Invalid => unreachable!(),
            command::AnimationAction::Stop => animation::State::Stopped,
            command::AnimationAction::RunWait => animation::State::Loading,
            command::AnimationAction::Run => animation::State::Running,
        };
        if old == animation::State::Stopped && anim.state != animation::State::Stopped {
            anim.frame_shown_at_ms = None;
        }
        anim.current_loop = 0;
        mutated = true;
    }

    // Set the loop count (v=), stored off by one per the protocol:
    // v=1 loops forever, v=n plays n-1 loops.
    if a.loops != 0 {
        anim.max_loops = a.loops - 1;
        mutated = true;
    }

    // (Zig marks each change as it is applied; the stamps are only
    // observable afterwards, so marking once in the same order is
    // equivalent.)
    if content_changed {
        storage.mark_image_content_changed(image_id);
    }
    if mutated {
        storage.mark_mutated();
    }

    // Successful animation control commands never respond.
    Response::default()
}

/// Compose a rectangle of pixels from one animation frame onto
/// another (a=c). Frame 1 (the root frame) always exists, so this also works
/// on images without any animation state.
fn compose_animation(terminal: &mut Terminal, cmd: &Command) -> Response {
    let Control::ComposeAnimation(c) = cmd.control else {
        unreachable!()
    };
    let storage = &mut terminal.screens.active.kitty_images;

    let mut result = Response {
        id: c.image_id,
        image_number: c.image_number,
        placement_id: c.placement_id,
        ..Default::default()
    };
    if c.image_id == 0 && c.image_number == 0 {
        result.message = "EINVAL: image ID or number required";
        return result;
    }
    let Some(img) = storage.image_ptr_by_id_or_number(c.image_id, c.image_number) else {
        result.message = "ENOENT: image not found";
        return result;
    };
    result.id = img.id;
    let image_id = img.id;
    if img.data.bytes().is_none() {
        result.message = "EINVAL: image data incomplete";
        return result;
    }

    // Both frames must exist. Note that r is the source frame and c
    // the destination; the spec's reference table describes these
    // backwards (see AnimationFrameComposition).
    if img.frame_data(c.source_frame).is_none() {
        result.message = "ENOENT: source frame not found";
        return result;
    }
    if img.frame_data(c.dest_frame).is_none() {
        result.message = "ENOENT: destination frame not found";
        return result;
    }

    // Rectangle validation is done in u64 so untrusted 32-bit values
    // can't overflow. Out-of-bounds rectangles are errors here,
    // unlike a=f which clips.
    let width: u64 = if c.width > 0 { c.width as u64 } else { img.width as u64 };
    let height: u64 = if c.height > 0 { c.height as u64 } else { img.height as u64 };
    if c.x as u64 + width > img.width as u64 || c.y as u64 + height > img.height as u64 {
        result.message = "EINVAL: destination rectangle out of bounds";
        return result;
    }
    if c.left_edge as u64 + width > img.width as u64 || c.top_edge as u64 + height > img.height as u64 {
        result.message = "EINVAL: source rectangle out of bounds";
        return result;
    }

    // Composing a frame onto itself requires non-overlapping
    // rectangles.
    if c.source_frame == c.dest_frame {
        let x_overlaps = (c.left_edge.max(c.x) as u64) < c.left_edge.min(c.x) as u64 + width;
        let y_overlaps = (c.top_edge.max(c.y) as u64) < c.top_edge.min(c.y) as u64 + height;
        if x_overlaps && y_overlaps {
            result.message = "EINVAL: source and destination rectangles overlap";
            return result;
        }
    }

    // All composition happens in RGBA.
    if let Err(err) = storage.convert_image_to_rgba(image_id) {
        encode_error(&mut result, alloc_error(err));
        return result;
    }

    // The frame data is owned by this storage. The rectangles were
    // validated disjoint above, so in-place composition within one
    // frame is well-defined.
    let img = storage.images.get_mut(&image_id).unwrap();
    let canvas_width = img.width;
    if c.source_frame == c.dest_frame {
        let canvas = img.frame_data_mut(c.dest_frame).unwrap();
        pixel::compose_canvas_rect_in_place(
            canvas,
            canvas_width,
            width as u32,
            height as u32,
            c.left_edge,
            c.top_edge,
            c.x,
            c.y,
            c.composition_mode,
        );
    } else {
        let src: *const [u8] = img.frame_data(c.source_frame).unwrap();
        let dst = img.frame_data_mut(c.dest_frame).unwrap();
        // SAFETY: the source and destination are different frames, so
        // they are distinct heap buffers owned by the image; neither is
        // reallocated while composing.
        let src = unsafe { &*src };
        pixel::compose_canvas_rect(
            dst,
            src,
            canvas_width,
            width as u32,
            height as u32,
            c.left_edge,
            c.top_edge,
            c.x,
            c.y,
            c.composition_mode,
        );
    }

    // If the destination is the displayed frame then the on-screen
    // content changed.
    let current: u32 = img.animation.as_ref().map_or(0, |anim| anim.current_index);
    if c.dest_frame - 1 == current {
        storage.mark_image_content_changed(image_id);
    } else {
        storage.mark_mutated();
    }

    result
}

/// Get or lazily create the animation state for an image.
fn ensure_animation(img: &mut Image) -> &mut animation::Animation {
    img.animation
        .get_or_insert_with(|| Box::new(animation::Animation::default()))
}

/// Execute a delete command.
fn delete(terminal: &mut Terminal, cmd: &Command) -> Response {
    let info = terminal_info(terminal);
    let screen: &mut Screen = &mut terminal.screens.active;

    // Every delete command aborts an incomplete chunked upload.
    screen.kitty_images.loading = None;

    // Then perform the actual deletion request, too.
    let Control::Delete(d) = cmd.control else {
        unreachable!()
    };
    screen.kitty_images.delete(&mut screen.pages, &info, d.action);

    // Delete never responds on success
    Response::default()
}

/// The result of [`load_and_add_image`] (Zig returns the image itself;
/// only its ID and implicit-ID flag are used).
struct LoadResult {
    image_id: u32,
    implicit_id: bool,
    more: bool,
    display: Option<command::Display>,
}

fn load_and_add_image(terminal: &mut Terminal, cmd: &Command) -> Result<LoadResult, image::Error> {
    let t = cmd.transmission().unwrap();
    let info = terminal_info(terminal);
    let screen: &mut Screen = &mut terminal.screens.active;
    let storage = &mut screen.kitty_images;

    // Determine our image. This also handles chunking and early exit.
    let mut loading: Box<LoadingImage> = if let Some(loading) = storage.loading.as_mut() {
        // Note: we do NOT want to take the command data here because
        // we're _copying_ the data. We want the command data to be freed.
        loading.add_data(&cmd.data)?;

        // If we have more then we're done
        if t.more_chunks {
            return Ok(LoadResult {
                image_id: loading.image.id,
                implicit_id: loading.image.metadata.implicit_id,
                more: true,
                display: None,
            });
        }

        // We have no more chunks. We're going to be completing the
        // image so we take the loading image out of the storage.
        storage.loading.take().unwrap()
    } else {
        // Reusing a specific image ID deletes the old image and all of its
        // placements when the new transmission begins, not when it completes.
        if t.image_id > 0 {
            storage.delete(
                &mut screen.pages,
                &info,
                command::DeleteAction::Id(command::DeleteId {
                    image_id: t.image_id,
                    delete: true,
                    ..Default::default()
                }),
            );
        }

        Box::new(LoadingImage::init(cmd, &storage.image_limits)?)
    };

    // If the image has no ID, we assign one
    if loading.image.id == 0 {
        if loading.image.number > 0 {
            loading.image.id = storage.next_image_id(NextImageIdMode::Explicit);
        } else {
            loading.image.id = storage.next_image_id(NextImageIdMode::Implicit);
            loading.image.metadata.implicit_id = true;
        }
    }

    // If this is chunked, this is the beginning of a new chunked transmission.
    // (We checked for an in-progress chunk above.)
    if t.more_chunks {
        // We box the loading image because its rare and we don't want to
        // always pay the memory cost to keep it around.
        let result = LoadResult {
            image_id: loading.image.id,
            implicit_id: loading.image.metadata.implicit_id,
            more: true,
            display: None,
        };
        storage.loading = Some(loading);
        return Ok(result);
    }

    // Dump the image data before it is decompressed
    // loading.debug_dump().unwrap();

    // Validate and store our image
    let img = loading.complete()?;
    let image_id = img.id;
    let implicit_id = img.metadata.implicit_id;
    storage
        .add_image(&mut screen.pages, img)
        .map_err(alloc_error)?;

    // Get our display settings
    let display_ = loading.display;

    Ok(LoadResult {
        image_id,
        implicit_id,
        more: false,
        display: display_,
    })
}

/// `Allocator.Error` as an image loading error.
fn alloc_error(_: AllocError) -> image::Error {
    image::Error::OutOfMemory
}

/// Encode an error code into a message for a response.
fn encode_error(r: &mut Response, err: image::Error) {
    r.message = match err {
        image::Error::OutOfMemory => "ENOMEM: out of memory",
        image::Error::InsufficientData => "ENODATA: insufficient data",
        image::Error::InvalidData => "EINVAL: invalid data",
        image::Error::DecompressionFailed => "EINVAL: decompression failed",
        image::Error::FilePathTooLong => "EINVAL: file path too long",
        image::Error::TemporaryFileNotInTempDir => "EINVAL: temporary file not in temp dir",
        image::Error::TemporaryFileNotNamedCorrectly => "EINVAL: temporary file not named correctly",
        image::Error::UnsupportedFormat => "EINVAL: unsupported format",
        image::Error::UnsupportedMedium => "EINVAL: unsupported medium",
        image::Error::UnsupportedDepth => "EINVAL: unsupported pixel depth",
        image::Error::DimensionsRequired => "EINVAL: dimensions required",
        image::Error::DimensionsTooLarge => "EINVAL: dimensions too large",
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::terminal::Options;
    use crate::terminal::kitty::graphics_command::{Parser, Quiet, Transmission};
    use crate::terminal::kitty::graphics_image::test_util::TmpDir;
    use crate::terminal::kitty::graphics_storage::ImageStorage;
    use crate::terminal::page_list::PinPtr;
    use crate::terminal::point;

    fn init(cols: u16, rows: u16) -> Terminal {
        Terminal::init(Options::new(cols, rows)).unwrap()
    }

    fn parse(input: &str) -> Command {
        Parser::parse_string(input.as_bytes()).unwrap()
    }

    fn exec(t: &mut Terminal, input: &str) -> Option<Response> {
        execute(t, &parse(input))
    }

    fn storage(t: &Terminal) -> &ImageStorage {
        &t.screens.active.kitty_images
    }

    fn storage_mut(t: &mut Terminal) -> &mut ImageStorage {
        &mut t.screens.active.kitty_images
    }

    fn tracked(t: &Terminal) -> usize {
        t.screens.active.pages.count_tracked_pins()
    }

    fn encode(resp: &Response) -> Vec<u8> {
        let mut buf = Vec::new();
        resp.encode(&mut buf).unwrap();
        buf
    }

    fn ext(image_id: u32, id: u32) -> PlacementKey {
        PlacementKey {
            image_id,
            placement_id: PlacementId {
                tag: PlacementTag::External,
                id,
            },
        }
    }

    fn pin_of(p: &Placement) -> PinPtr {
        match p.location {
            Location::Pin(pin) => pin,
            Location::Virtual | Location::Relative(_) => unreachable!(),
        }
    }

    fn anim_of(t: &mut Terminal) -> &mut animation::Animation {
        storage_mut(t)
            .image_ptr_by_id_or_number(1, 0)
            .unwrap()
            .animation
            .as_mut()
            .unwrap()
    }

    /// Writes `data` to a file in `dir` and returns the file's absolute path,
    /// base64 encoded. The result is ready to use as the payload of a file
    /// (t=f) transmission.
    fn test_file_payload(dir: &TmpDir, data: &[u8]) -> String {
        use base64::Engine as _;
        dir.write_file("image.data", data);
        let path = dir.real_path_file("image.data");
        base64::engine::general_purpose::STANDARD.encode(path)
    }

    #[test]
    fn kittygfx_query_validates_image_data() {
        let mut terminal = init(5, 5);

        let cmd = Command {
            control: Control::Query(Transmission {
                format: command::Format::Rgb,
                width: 1,
                height: 1,
                image_id: 31,
                ..Default::default()
            }),
            quiet: Quiet::No,
            // A 1x1 RGB image requires three bytes.
            data: vec![0, 0],
        };

        let resp = execute(&mut terminal, &cmd).unwrap();
        assert!(!resp.ok());
        assert_eq!(31u32, resp.id);
        assert_eq!("EINVAL: invalid data", resp.message);
        assert_eq!(0, storage(&terminal).images.len());
    }

    #[test]
    fn kittygfx_valid_query_does_not_replace_or_store_image() {
        let mut terminal = init(5, 5);

        // Store a red pixel under the same ID used by the query.
        assert!(exec(&mut terminal, "a=t,f=24,s=1,v=1,i=31;/wAA").unwrap().ok());

        // Successfully validate a black pixel without replacing the red one.
        let cmd = Command {
            control: Control::Query(Transmission {
                format: command::Format::Rgb,
                width: 1,
                height: 1,
                image_id: 31,
                ..Default::default()
            }),
            quiet: Quiet::No,
            data: vec![0, 0, 0],
        };

        let resp = execute(&mut terminal, &cmd).unwrap();
        assert!(resp.ok());
        assert_eq!(1, storage(&terminal).images.len());
        assert_eq!(
            &[255u8, 0, 0][..],
            storage(&terminal).image_by_id(31).unwrap().data.bytes().unwrap()
        );
    }

    #[test]
    fn kittygfx_image_id_and_number_are_mutually_exclusive_for_every_action() {
        let mut t = init(5, 5);

        let inputs = [
            "a=q,f=24,s=1,v=1,i=1,I=2,p=3;AAAA",
            "a=t,f=24,s=1,v=1,i=1,I=2,p=3;AAAA",
            "a=T,f=24,s=1,v=1,i=1,I=2,p=3;AAAA",
            "a=p,i=1,I=2,p=3",
            "a=d,d=a,i=1,I=2,p=3",
            "a=f,i=1,I=2,p=3;AAAA",
            "a=a,i=1,I=2,p=3",
            "a=c,i=1,I=2,p=3",
        ];

        for input in inputs {
            let resp = exec(&mut t, input).unwrap();
            assert!(!resp.ok());
            assert_eq!(1u32, resp.id);
            assert_eq!(2u32, resp.image_number);
            assert_eq!(3u32, resp.placement_id);
            assert_eq!("EINVAL: image ID and number are mutually exclusive", resp.message);

            assert_eq!(
                &b"\x1b_Gi=1,I=2,p=3;EINVAL: image ID and number are mutually exclusive\x1b\\"[..],
                &encode(&resp)[..]
            );
        }

        assert_eq!(0, storage(&t).images.len());
        assert_eq!(0, storage(&t).placements.len());
    }

    #[test]
    fn kittygfx_conflicting_identifiers_are_rejected_before_mutation() {
        let mut t = init(5, 5);

        // Store an image so a buggy put would succeed and mutate placement state.
        assert!(exec(&mut t, "a=t,f=24,s=1,v=1,i=1;/wAA").unwrap().ok());

        // Directly constructed commands must receive the same validation as
        // commands produced by the parser.
        {
            let cmd = Command {
                control: Control::Display(command::Display {
                    image_id: 1,
                    image_number: 2,
                    placement_id: 7,
                    cursor_movement: command::CursorMovement::None,
                    ..Default::default()
                }),
                quiet: Quiet::No,
                data: Vec::new(),
            };
            let resp = execute(&mut t, &cmd).unwrap();
            assert!(!resp.ok());
            assert_eq!(1u32, resp.id);
            assert_eq!(2u32, resp.image_number);
            assert_eq!(7u32, resp.placement_id);
            assert_eq!(0, storage(&t).placements.len());
        }

        // Add a real placement so a buggy delete would remove it.
        assert!(exec(&mut t, "a=p,i=1,p=7,C=1").unwrap().ok());
        assert_eq!(1, storage(&t).placements.len());

        {
            let resp = exec(&mut t, "a=d,d=i,i=1,I=2,p=7").unwrap();
            assert!(!resp.ok());
            assert_eq!(1, storage(&t).placements.len());
        }

        // q=2 suppresses the required error response but not the validation.
        assert!(exec(&mut t, "a=d,d=i,i=1,I=2,p=7,q=2").is_none());
        assert_eq!(1, storage(&t).placements.len());
    }

    #[test]
    fn kittygfx_chunked_success_response_uses_initial_identifiers() {
        let mut t = init(5, 5);

        assert!(exec(&mut t, "a=t,f=24,s=1,v=2,I=93,p=7,m=1;AAAA").is_none());

        let resp = exec(&mut t, "m=0;AAAA").unwrap();
        assert!(resp.ok());
        assert_eq!(1u32, resp.id);
        assert_eq!(93u32, resp.image_number);
        assert_eq!(7u32, resp.placement_id);
        assert_eq!(&b"\x1b_Gi=1,I=93,p=7;OK\x1b\\"[..], &encode(&resp)[..]);
    }

    #[test]
    fn kittygfx_chunked_error_response_uses_initial_identifiers() {
        let mut t = init(5, 5);

        assert!(exec(&mut t, "a=t,f=24,s=1,v=1,i=41,p=7,m=1;AA==").is_none());

        let resp = exec(&mut t, "m=0;AA==").unwrap();
        assert!(!resp.ok());
        assert_eq!(41u32, resp.id);
        assert_eq!(7u32, resp.placement_id);
        assert_eq!("EINVAL: invalid data", resp.message);
    }

    #[test]
    fn kittygfx_more_chunks_with_q_1() {
        let mut t = init(5, 5);

        // Initial chunk has q=1
        assert!(exec(&mut t, "a=T,f=24,t=d,i=1,s=1,v=2,c=10,r=1,m=1,q=1;////").is_none());

        // Subsequent chunk has no q but should respect initial
        assert!(exec(&mut t, "m=0;////").is_none());
    }

    #[test]
    fn kittygfx_more_chunks_with_q_0() {
        let mut t = init(5, 5);

        // Initial chunk has q=0
        assert!(exec(&mut t, "a=t,f=24,t=d,s=1,v=2,c=10,r=1,m=1,i=1,q=0;////").is_none());

        // Subsequent chunk has no q so should respond OK
        let resp = exec(&mut t, "m=0;////").unwrap();
        assert!(resp.ok());
    }

    #[test]
    fn kittygfx_more_chunks_with_chunk_increasing_q() {
        let mut t = init(5, 5);

        // Initial chunk has q=0
        assert!(exec(&mut t, "a=t,f=24,t=d,s=1,v=2,c=10,r=1,m=1,i=1,q=0;////").is_none());

        // Subsequent chunk sets q=1 so should not respond
        assert!(exec(&mut t, "m=0,q=1;////").is_none());
    }

    #[test]
    fn kittygfx_delete_aborts_chunked_image_load() {
        let mut t = init(5, 5);

        // Begin a two-chunk RGB image, then interrupt it with a delete.
        assert!(exec(&mut t, "a=t,f=24,s=1,v=2,i=1,m=1;AAAA").is_none());
        assert!(storage(&t).loading.is_some());

        assert!(exec(&mut t, "a=d").is_none());
        assert!(storage(&t).loading.is_none());

        // A fresh chunked upload must start from empty state after the delete.
        assert!(exec(&mut t, "a=t,f=24,s=1,v=2,i=1,m=1;AAAA").is_none());
        assert!(exec(&mut t, "m=0;AAAA").unwrap().ok());
        assert_eq!(6usize, storage(&t).image_by_id(1).unwrap().data.len());
    }

    #[test]
    fn kittygfx_non_direct_transmission_abandons_chunked_image_load() {
        let mut t = init(5, 5);

        // Allow file (t=f) transmissions and write a 1x1 RGB image file.
        storage_mut(&mut t).image_limits.file = true;
        let tmp_dir = TmpDir::new();
        let payload = test_file_payload(&tmp_dir, &[1, 2, 3]);

        // Begin a two-chunk RGB image.
        assert!(exec(&mut t, "a=t,f=24,s=1,v=2,i=1,m=1;AAAA").is_none());
        assert!(storage(&t).loading.is_some());

        // A file transmission can't be a continuation chunk, so it abandons
        // the load and transmits a new image from the file.
        {
            let str = format!("a=t,t=f,f=24,s=1,v=1,i=2;{payload}");
            let resp = exec(&mut t, &str).unwrap();
            assert!(resp.ok());
            assert_eq!(2u32, resp.id);
        }
        assert!(storage(&t).loading.is_none());
        assert!(storage(&t).image_by_id(1).is_none());
        assert_eq!(
            &[1u8, 2, 3][..],
            storage(&t).image_by_id(2).unwrap().data.bytes().unwrap()
        );
    }

    #[test]
    fn kittygfx_uppercase_id_delete_preserves_image_when_placement_does_not_match() {
        let mut t = init(5, 5);

        // Store an unplaced 1x1 RGB image.
        assert!(exec(&mut t, "a=t,f=24,s=1,v=1,i=1;AAAA").unwrap().ok());

        // Uppercase deletion may free data only if the named placement matched.
        assert!(exec(&mut t, "a=d,d=I,i=1,p=7").is_none());

        assert!(storage(&t).image_by_id(1).is_some());
    }

    #[test]
    fn kittygfx_default_format_is_rgba() {
        let mut t = init(5, 5);

        let resp = exec(&mut t, "a=t,t=d,i=1,s=1,v=2,c=10,r=1;///////////").unwrap();
        assert!(resp.ok());

        let img = storage(&t).image_by_id(1).unwrap();
        assert_eq!(command::Format::Rgba, img.format);
    }

    #[test]
    fn kittygfx_test_valid_u32_expect_invalid_image_id() {
        let mut t = init(5, 5);

        let resp = exec(&mut t, "a=p,i=4294967295").unwrap();
        assert!(!resp.ok());
        assert_eq!(resp.message, "ENOENT: image not found");
    }

    #[test]
    fn kittygfx_test_valid_i32_expect_invalid_image_id() {
        let mut t = init(5, 5);

        let resp = exec(&mut t, "a=p,i=1,z=-2147483648").unwrap();
        assert!(!resp.ok());
        assert_eq!(resp.message, "ENOENT: image not found");
    }

    #[test]
    fn kittygfx_no_response_with_no_image_id_or_number() {
        let mut t = init(5, 5);

        assert!(exec(&mut t, "a=t,f=24,t=d,s=1,v=2,c=10,r=1,i=0,I=0;////////").is_none());
    }

    #[test]
    fn kittygfx_no_response_with_no_image_id_or_number_load_and_display() {
        let mut t = init(5, 5);

        assert!(exec(&mut t, "a=T,f=24,t=d,s=1,v=2,c=10,r=1,i=0,I=0;////////").is_none());
    }

    #[test]
    fn kittygfx_retransmit_same_id_gets_fresh_image_generation() {
        let mut t = init(5, 5);

        // Transmit a 1x2 RGB image with id=1.
        assert!(exec(&mut t, "a=t,t=d,f=24,i=1,s=1,v=2;////////").unwrap().ok());
        let gen1 = storage(&t).image_by_id(1).unwrap().generation;
        assert!(gen1 > 0);
        assert_eq!(gen1, storage(&t).generation);

        // Retransmit the same id with identical dimensions/length. The
        // (width, height, format, len) tuple is identical, so only the
        // generation can reveal that the contents were replaced.
        assert!(exec(&mut t, "a=t,t=d,f=24,i=1,s=1,v=2;AAAAAAAA").unwrap().ok());
        let gen2 = storage(&t).image_by_id(1).unwrap().generation;
        assert!(gen2 > gen1);
        assert_eq!(gen2, storage(&t).generation);
    }

    #[test]
    fn kittygfx_retransmit_same_id_removes_existing_placements() {
        let mut t = init(5, 5);
        let before = tracked(&t);

        // Transmit and display an image, then add anonymous and named placements.
        // Multiple anonymous a=p placements for one image are explicitly valid.
        assert!(exec(&mut t, "a=T,t=d,f=24,i=1,s=1,v=2,C=1;////////").unwrap().ok());
        assert!(exec(&mut t, "a=p,i=1,C=1").unwrap().ok());
        assert!(exec(&mut t, "a=p,i=1,p=7,C=1").unwrap().ok());
        assert_eq!(3, storage(&t).placements.len());
        assert_eq!(before + 3, tracked(&t));

        // Retransmitting replaces the image and must delete every old placement.
        // Plain a=t creates no replacement placement of its own.
        assert!(exec(&mut t, "a=t,t=d,f=24,i=1,s=1,v=2;AAAAAAAA").unwrap().ok());
        assert_eq!(0, storage(&t).placements.len());
        assert_eq!(before, tracked(&t));

        // a=T creates one new placement after deleting the previous image and
        // placements, so repeated redraws remain bounded at one placement.
        assert!(exec(&mut t, "a=T,t=d,f=24,i=1,s=1,v=2,C=1;////////").unwrap().ok());
        assert!(exec(&mut t, "a=T,t=d,f=24,i=1,s=1,v=2,C=1;AAAAAAAA").unwrap().ok());
        assert_eq!(1, storage(&t).placements.len());
        assert_eq!(before + 1, tracked(&t));
    }

    #[test]
    fn kittygfx_retransmit_same_id_removes_image_on_first_chunk() {
        let mut t = init(5, 5);
        let before = tracked(&t);

        // Store and display the old image.
        assert!(exec(&mut t, "a=T,t=d,f=24,i=1,s=1,v=2,C=1;////////").unwrap().ok());
        assert!(storage(&t).image_by_id(1).is_some());
        assert_eq!(1, storage(&t).placements.len());
        assert_eq!(before + 1, tracked(&t));

        // Starting a replacement for the same explicit ID removes the old image
        // and placements before the replacement's final chunk arrives.
        assert!(exec(&mut t, "a=t,t=d,f=24,i=1,s=1,v=2,m=1;AAAA").is_none());
        assert!(storage(&t).loading.is_some());
        assert!(storage(&t).image_by_id(1).is_none());
        assert_eq!(0, storage(&t).placements.len());
        assert_eq!(before, tracked(&t));
    }

    #[test]
    fn kittygfx_delete_then_retransmit_same_id_gets_fresh_generation() {
        let mut t = init(5, 5);

        // Transmit and display, then delete everything (including image
        // data), then retransmit the same ID.
        assert!(exec(&mut t, "a=T,t=d,f=24,i=1,s=1,v=2,c=1,r=1;////////").unwrap().ok());
        let gen1 = storage(&t).image_by_id(1).unwrap().generation;

        assert!(exec(&mut t, "a=d,d=A").is_none());
        assert!(storage(&t).image_by_id(1).is_none());
        let gen_delete = storage(&t).generation;
        assert!(gen_delete > gen1);

        assert!(exec(&mut t, "a=t,t=d,f=24,i=1,s=1,v=2;////////").unwrap().ok());
        let gen2 = storage(&t).image_by_id(1).unwrap().generation;
        assert!(gen2 > gen1);
        assert!(gen2 > gen_delete);
    }

    #[test]
    fn kittygfx_display_clamps_cell_offsets() {
        let mut t = init(5, 5);
        t.width_px = 50; // 10 px per col
        t.height_px = 100; // 20 px per row

        let resp = exec(&mut t, "a=T,t=d,f=24,i=1,s=1,v=1,c=2,r=1,X=99,Y=99,C=1;AAAA").unwrap();
        assert!(resp.ok());

        let info = terminal_info(&t);
        let storage = storage(&t);
        let placement = storage.placements.values().next().unwrap();
        assert_eq!(9u32, placement.x_offset);
        assert_eq!(19u32, placement.y_offset);

        let actual = placement.pixel_size(storage.image_by_id(1).unwrap(), &info);
        assert_eq!(11u32, actual.width);
        assert_eq!(1u32, actual.height);
    }

    #[test]
    fn kittygfx_placement_bounds_cursor_movement_for_untrusted_dimensions() {
        let mut t = init(5, 5);

        let resp = exec(&mut t, "a=T,t=d,f=24,i=1,s=1,v=1,c=4294967295,r=4294967295;////").unwrap();
        assert!(resp.ok());
        assert_eq!(1, storage(&t).placements.len());
        // Reaching the bottom takes four rows, then scrolling is capped to one
        // screen (five rows), matching Kitty's bounded screen-scroll behavior.
        assert_eq!(10usize, t.screens.active.pages.scrollbar().total);
    }

    #[test]
    fn kittygfx_placement_moves_cursor_past_a_tall_image() {
        let mut t = init(5, 5);

        // Load a one-pixel RGB image, then place it over the full width and eight
        // rows. Eight rows are taller than the screen but still within Kitty's
        // scroll budget.
        assert!(exec(&mut t, "a=t,t=d,f=24,i=1,s=1,v=1;////").unwrap().ok());
        assert!(exec(&mut t, "a=p,i=1,p=1,c=5,r=8").unwrap().ok());

        // A subsequent placement begins immediately after the first one instead
        // of overlapping it at the old one-screen movement cap.
        assert!(exec(&mut t, "a=p,i=1,p=2,C=1").unwrap().ok());

        let first_pin = pin_of(&storage(&t).placements[&ext(1, 1)]);
        let second_pin = pin_of(&storage(&t).placements[&ext(1, 2)]);
        let pages = &t.screens.active.pages;
        // SAFETY: both pins are tracked by the active screen.
        let first_y = pages
            .point_from_pin(point::Tag::Screen, unsafe { *first_pin.as_ptr() })
            .unwrap()
            .coord()
            .y;
        // SAFETY: as above.
        let second_y = pages
            .point_from_pin(point::Tag::Screen, unsafe { *second_pin.as_ptr() })
            .unwrap()
            .coord()
            .y;
        assert_eq!(first_y + 8, second_y);
    }

    #[test]
    fn kittygfx_unknown_format_responds_with_einval() {
        let mut t = init(5, 5);

        let resp = exec(&mut t, "a=t,f=42,t=d,i=31,s=1,v=1;AAAA").unwrap();
        assert!(!resp.ok());
        assert_eq!(31u32, resp.id);
        assert_eq!("EINVAL: unsupported format", resp.message);
        assert_eq!(0, storage(&t).images.len());
    }

    #[test]
    fn kittygfx_unknown_format_on_query_responds_with_einval() {
        let mut t = init(5, 5);

        let resp = exec(&mut t, "a=q,f=42,t=d,i=31,s=1,v=1;AAAA").unwrap();
        assert!(!resp.ok());
        assert_eq!(31u32, resp.id);
        assert_eq!("EINVAL: unsupported format", resp.message);
    }

    #[test]
    fn kittygfx_zero_format_is_rgba() {
        let mut t = init(5, 5);

        // Kitty treats f=0 as an absent f, i.e. RGBA.
        let resp = exec(&mut t, "a=t,f=0,t=d,i=1,s=1,v=2,c=10,r=1;///////////").unwrap();
        assert!(resp.ok());

        let img = storage(&t).image_by_id(1).unwrap();
        assert_eq!(command::Format::Rgba, img.format);
    }

    #[test]
    fn kittygfx_unknown_format_with_q_3_has_no_response() {
        let mut t = init(5, 5);

        // q above 2 is out of range in the spec but Kitty suppresses
        // everything for anything above 1, so we must still parse it.
        assert!(exec(&mut t, "a=t,f=42,t=d,i=31,q=3,s=1,v=1;AAAA").is_none());
    }

    #[test]
    fn kittygfx_out_of_range_display_keys_are_tolerated() {
        let mut t = init(5, 5);

        {
            let resp = exec(&mut t, "a=T,f=24,t=d,i=31,s=1,v=1,C=2,U=2;AAAA").unwrap();
            assert!(resp.ok());
            assert_eq!(31u32, resp.id);
        }

        // U=2 must behave like U=1, so the placement is virtual.
        let entry = storage(&t).placements.values().next().unwrap();
        assert!(entry.location == Location::Virtual);
    }

    #[test]
    fn kittygfx_number_based_transmission_assigns_smallest_free_id() {
        let mut t = init(5, 5);

        // Empty storage: the first free ID is 1, as in Kitty.
        {
            let resp = exec(&mut t, "a=t,f=24,s=1,v=1,I=42;/wAA").unwrap();
            assert!(resp.ok());
            assert_eq!(1u32, resp.id);
            assert_eq!(42u32, resp.image_number);
        }

        // Occupy ID 2 explicitly; the next number gets 3.
        assert!(exec(&mut t, "a=t,f=24,s=1,v=1,i=2;/wAA").unwrap().ok());
        {
            let resp = exec(&mut t, "a=t,f=24,s=1,v=1,I=43;/wAA").unwrap();
            assert!(resp.ok());
            assert_eq!(3u32, resp.id);
        }

        // Deleting ID 1 opens a gap that the next number fills.
        assert!(exec(&mut t, "a=d,d=I,i=1").is_none());
        {
            let resp = exec(&mut t, "a=t,f=24,s=1,v=1,I=44;/wAA").unwrap();
            assert!(resp.ok());
            assert_eq!(1u32, resp.id);
        }

        let storage = storage(&t);
        assert_eq!(3, storage.images.len());
        assert_eq!(44u32, storage.image_by_id(1).unwrap().number);
        assert_eq!(0u32, storage.image_by_id(2).unwrap().number);
        assert_eq!(43u32, storage.image_by_id(3).unwrap().number);
    }

    #[test]
    fn kittygfx_number_based_id_assignment_does_not_replace_client_image() {
        let mut t = init(5, 5);

        // A client stores and places a red pixel on the ID the old wrapping
        // counter would assign first.
        assert!(exec(&mut t, "a=t,f=24,s=1,v=1,i=2147483647;/wAA").unwrap().ok());
        assert!(exec(&mut t, "a=p,i=2147483647,C=1").unwrap().ok());

        // A number-based transmission must not collide with it.
        {
            let resp = exec(&mut t, "a=t,f=24,s=1,v=1,I=42;AAD/").unwrap();
            assert!(resp.ok());
            assert_eq!(1u32, resp.id);
        }

        // The client's image and placement are untouched.
        let storage = storage(&t);
        assert_eq!(2, storage.images.len());
        assert_eq!(1, storage.placements.len());
        assert_eq!(
            &[255u8, 0, 0][..],
            storage.image_by_id(2147483647).unwrap().data.bytes().unwrap()
        );
    }

    #[test]
    fn kittygfx_implicit_id_assignment_does_not_replace_client_image() {
        let mut t = init(5, 5);

        // A client stores and places a red pixel on the ID the implicit
        // counter assigns first.
        assert!(exec(&mut t, "a=t,f=24,s=1,v=1,i=2147483647;/wAA").unwrap().ok());
        assert!(exec(&mut t, "a=p,i=2147483647,C=1").unwrap().ok());

        // Transmit without an ID or number: no response, and the counter
        // skips over the in-use ID instead of replacing that image.
        assert!(exec(&mut t, "a=t,f=24,s=1,v=1;AAD/").is_none());

        let storage = storage(&t);
        assert_eq!(2, storage.images.len());
        assert_eq!(1, storage.placements.len());
        assert_eq!(
            &[255u8, 0, 0][..],
            storage.image_by_id(2147483647).unwrap().data.bytes().unwrap()
        );
        let implicit = storage.image_by_id(2147483648).unwrap();
        assert!(implicit.metadata.implicit_id);
        assert_eq!(&[0u8, 0, 255][..], implicit.data.bytes().unwrap());
    }

    #[test]
    fn kittygfx_relative_placement_with_missing_parent_image() {
        let mut t = init(5, 5);

        assert!(exec(&mut t, "a=t,f=24,s=1,v=1,i=1;AAAA").unwrap().ok());

        // The parent image does not exist: the placement must be rejected
        // with ENOPARENT, nothing may be created, and the cursor must not
        // move (a relative placement never moves the cursor, and a failed
        // one certainly must not).
        {
            let resp = exec(&mut t, "a=p,i=1,p=1,P=42,Q=1,H=2,V=2").unwrap();
            assert!(!resp.ok());
            assert_eq!("ENOPARENT: parent image not found", resp.message);
        }

        assert_eq!(0, storage(&t).placements.len());
        assert_eq!(0, t.screens.active.cursor.x);
        assert_eq!(0, t.screens.active.cursor.y);
    }

    #[test]
    fn kittygfx_relative_placement_with_missing_parent_placement() {
        let mut t = init(5, 5);

        for input in ["a=t,f=24,s=1,v=1,i=1;AAAA", "a=t,f=24,s=1,v=1,i=2;AAAA"] {
            assert!(exec(&mut t, input).unwrap().ok());
        }

        // The parent image exists but has no placements at all.
        {
            let resp = exec(&mut t, "a=p,i=1,P=2").unwrap();
            assert!(!resp.ok());
            assert_eq!("ENOPARENT: parent placement not found", resp.message);
        }

        // The parent image has a placement, but not the requested one.
        assert!(exec(&mut t, "a=p,i=2,p=1,C=1").unwrap().ok());
        {
            let resp = exec(&mut t, "a=p,i=1,P=2,Q=9").unwrap();
            assert!(!resp.ok());
            assert_eq!("ENOPARENT: parent placement not found", resp.message);
        }

        assert_eq!(1, storage(&t).placements.len());
    }

    #[test]
    fn kittygfx_relative_placement_cannot_parent_itself() {
        let mut t = init(5, 5);

        for input in ["a=t,f=24,s=1,v=1,i=1;AAAA", "a=p,i=1,p=1,C=1"] {
            assert!(exec(&mut t, input).unwrap().ok());
        }

        // Explicitly via Q, and implicitly when the Q=0 fallback selects
        // the placement being replaced.
        for input in ["a=p,i=1,p=1,P=1,Q=1", "a=p,i=1,p=1,P=1"] {
            let resp = exec(&mut t, input).unwrap();
            assert!(!resp.ok());
            assert_eq!("EINVAL: placement cannot be its own parent", resp.message);
        }

        // The original placement must be untouched.
        let p = storage(&t).placements[&ext(1, 1)];
        assert!(matches!(p.location, Location::Pin(_)));
    }

    #[test]
    fn kittygfx_relative_placement_cycle() {
        let mut t = init(5, 5);

        for input in [
            "a=t,f=24,s=1,v=1,i=1;AAAA",
            "a=p,i=1,p=1,C=1",
            "a=p,i=1,p=2,P=1,Q=1",
        ] {
            assert!(exec(&mut t, input).unwrap().ok());
        }

        // Replacing placement 1 with a parent of placement 2 would create
        // the cycle 1 -> 2 -> 1.
        {
            let resp = exec(&mut t, "a=p,i=1,p=1,P=1,Q=2").unwrap();
            assert!(!resp.ok());
            assert_eq!("ECYCLE: parent chain creates a cycle", resp.message);
        }

        // The original placement must be untouched, keeping the stored
        // chains acyclic.
        let p = storage(&t).placements[&ext(1, 1)];
        assert!(matches!(p.location, Location::Pin(_)));
    }

    #[test]
    fn kittygfx_relative_placement_chain_depth_limit() {
        let mut t = init(5, 5);

        for input in ["a=t,f=24,s=1,v=1,i=1;AAAA", "a=p,i=1,p=1,C=1"] {
            assert!(exec(&mut t, input).unwrap().ok());
        }

        // Chains of up to parent_chain_limit (8) links must work: these
        // placements form the chain 9 -> 8 -> ... -> 2 -> 1.
        for id in 2..10 {
            let input = format!("a=p,i=1,p={},P=1,Q={}", id, id - 1);
            assert!(exec(&mut t, &input).unwrap().ok());
        }

        // One more link exceeds the limit.
        {
            let resp = exec(&mut t, "a=p,i=1,p=10,P=1,Q=9").unwrap();
            assert!(!resp.ok());
            assert_eq!("ETOODEEP: parent chain too deep", resp.message);
        }

        assert_eq!(9, storage(&t).placements.len());
    }

    #[test]
    fn kittygfx_relative_placement_does_not_move_cursor() {
        let mut t = init(5, 5);

        // C is left at its default (move after) on the relative placement
        // but it must never move the cursor.
        for input in [
            "a=t,f=24,s=1,v=1,i=1;AAAA",
            "a=p,i=1,p=1,C=1",
            "a=p,i=1,p=2,P=1,Q=1,H=3,V=2,c=2,r=2",
        ] {
            assert!(exec(&mut t, input).unwrap().ok());
        }
        assert_eq!(0, t.screens.active.cursor.x);
        assert_eq!(0, t.screens.active.cursor.y);

        // The stored placement carries the parent link and offsets.
        let p = storage(&t).placements[&ext(1, 2)];
        let Location::Relative(rel) = p.location else {
            panic!("not relative")
        };
        assert!(rel.parent.eql(ext(1, 1)));
        assert_eq!(3i32, rel.horizontal_offset);
        assert_eq!(2i32, rel.vertical_offset);
    }

    #[test]
    fn kittygfx_virtual_placement_with_parent_rejected_before_image_lookup() {
        let mut t = init(5, 5);

        // Kitty rejects U=1 + P= before checking that the image exists,
        // so this must not be ENOENT.
        let resp = exec(&mut t, "a=p,i=42,U=1,P=1").unwrap();
        assert!(!resp.ok());
        assert_eq!("EINVAL: virtual placement cannot refer to a parent", resp.message);
    }

    #[test]
    fn kittygfx_deleting_a_parent_deletes_relative_placements_transitively() {
        let mut t = init(5, 5);

        // Root (pin) <- child <- grandchild
        for input in [
            "a=t,f=24,s=1,v=1,i=1;AAAA",
            "a=p,i=1,p=1,C=1",
            "a=p,i=1,p=2,P=1,Q=1",
            "a=p,i=1,p=3,P=1,Q=2",
        ] {
            assert!(exec(&mut t, input).unwrap().ok());
        }
        assert_eq!(3, storage(&t).placements.len());

        // Deleting the middle placement takes the grandchild with it but
        // leaves the root.
        assert!(exec(&mut t, "a=d,d=i,i=1,p=2").is_none());
        assert_eq!(1, storage(&t).placements.len());
        assert!(storage(&t).placements.contains_key(&ext(1, 1)));

        // Deleting the root removes the remaining placement, and the
        // image itself is retained (lowercase delete).
        assert!(exec(&mut t, "a=d,d=i,i=1,p=1").is_none());
        assert_eq!(0, storage(&t).placements.len());
        assert!(storage(&t).image_by_id(1).is_some());
    }

    #[test]
    fn kittygfx_retransmitting_parent_image_deletes_relative_placements() {
        let mut t = init(5, 5);

        // Image 2's placement is relative to image 1's placement.
        for input in [
            "a=t,f=24,s=1,v=1,i=1;AAAA",
            "a=t,f=24,s=1,v=1,i=2;AAAA",
            "a=p,i=1,p=1,C=1",
            "a=p,i=2,p=1,P=1,Q=1",
        ] {
            assert!(exec(&mut t, input).unwrap().ok());
        }
        assert_eq!(2, storage(&t).placements.len());

        // Retransmitting image 1 removes its placements, which orphans
        // and removes image 2's placement too. Image 2 is left without
        // any placement so it is freed as well: retransmission deletes
        // with uppercase semantics, and kitty also removes an image whose
        // last placement died from a broken parent chain.
        assert!(exec(&mut t, "a=t,f=24,s=1,v=1,i=1;AAAA").unwrap().ok());
        assert_eq!(0, storage(&t).placements.len());
        assert!(storage(&t).image_by_id(2).is_none());
    }

    #[test]
    fn kittygfx_relative_placement_parent_fallback_picks_lowest_external_id() {
        let mut t = init(5, 5);

        for input in [
            "a=t,f=24,s=1,v=1,i=1;AAAA",
            "a=t,f=24,s=1,v=1,i=2;AAAA",
            "a=p,i=1,p=7,C=1",
            "a=p,i=1,p=3,C=1",
            "a=p,i=2,P=1",
        ] {
            assert!(exec(&mut t, input).unwrap().ok());
        }

        let rel = storage(&t)
            .placements
            .iter()
            .find_map(|(k, p)| match p.location {
                Location::Relative(rel) if k.image_id == 2 => Some(rel),
                _ => None,
            })
            .expect("PlacementNotFound");
        assert!(rel.parent.eql(ext(1, 3)));
    }

    // Regression test: Screen.reset reuses the cursor's tracked pin
    // but PageList.reset marked it garbage. Placements copy the cursor
    // pin, so every placement created after a reset was born garbage
    // and silently swept by the next placement command.
    #[test]
    fn kittygfx_placements_created_after_a_full_reset_are_retained() {
        let mut t = init(80, 24);

        t.full_reset();

        for input in [
            "a=t,f=24,s=1,v=1,i=1;AAAA",
            "a=p,i=1,p=1,C=1",
            "a=p,i=1,p=2,C=1",
            "a=p,i=1,p=3,P=1,Q=2",
        ] {
            assert!(exec(&mut t, input).unwrap().ok());
        }
        assert_eq!(3, storage(&t).placements.len());
    }

    #[test]
    fn kittygfx_relative_placement_with_pruned_parent_is_enoparent() {
        let mut t = init(5, 5);

        for input in ["a=t,f=24,s=1,v=1,i=1;AAAA", "a=p,i=1,p=1,C=1"] {
            assert!(exec(&mut t, input).unwrap().ok());
        }

        // The parent's tracked content is pruned from history but the
        // placement hasn't been swept yet. The put must reap it and answer
        // ENOPARENT rather than storing an orphan against it.
        let parent = pin_of(&storage(&t).placements[&ext(1, 1)]);
        // SAFETY: the pin is tracked by the active screen.
        unsafe { (*parent.as_ptr()).garbage = true };

        {
            let resp = exec(&mut t, "a=p,i=1,p=2,P=1,Q=1").unwrap();
            assert!(!resp.ok());
            assert_eq!("ENOPARENT: parent placement not found", resp.message);
        }
        assert_eq!(0, storage(&t).placements.len());
    }

    #[test]
    fn kittygfx_uppercase_delete_frees_image_of_cascaded_placements() {
        let mut t = init(5, 5);

        for input in [
            "a=t,f=24,s=1,v=1,i=1;AAAA",
            "a=p,i=1,p=1,C=1",
            "a=p,i=1,p=2,P=1,Q=1",
        ] {
            assert!(exec(&mut t, input).unwrap().ok());
        }

        // The uppercase delete only matches placement 1. The cascade
        // removes placement 2, leaving the image without placements, so
        // the image must be freed too.
        assert!(exec(&mut t, "a=d,d=I,i=1,p=1").is_none());
        assert_eq!(0, storage(&t).placements.len());
        assert!(storage(&t).image_by_id(1).is_none());
    }

    // Animation tests frequently start from a 1x1 RGB red image with
    // id=1 ("/wAA" is FF0000). Composition converts images to RGBA, so
    // after the first frame command the base data is FF0000FF.

    #[test]
    fn kittygfx_animation_new_frame_with_default_gap_responds_with_frame_number() {
        let mut t = init(5, 5);
        assert!(exec(&mut t, "a=t,f=24,s=1,v=1,i=1;/wAA").unwrap().ok());

        // Transmit a 1x1 RGB blue frame ("AAD/" is 0000FF).
        let resp = exec(&mut t, "a=f,i=1,f=24,s=1,v=1;AAD/").unwrap();
        assert!(resp.ok());
        assert_eq!(1u32, resp.id);
        assert_eq!(2u32, resp.frame);

        assert_eq!(&b"\x1b_Gi=1,r=2;OK\x1b\\"[..], &encode(&resp)[..]);

        let img = storage_mut(&mut t).image_ptr_by_id_or_number(1, 0).unwrap();
        assert_eq!(command::Format::Rgba, img.format);
        assert_eq!(&[255u8, 0, 0, 255][..], img.data.bytes().unwrap());

        let anim = img.animation.as_ref().unwrap();
        assert_eq!(2u32, anim.frame_count());
        assert_eq!(0u32, anim.root_gap_ms);
        assert_eq!(40u32, anim.frames[0].gap_ms);
        assert_eq!(&[0u8, 0, 255, 255][..], &anim.frames[0].data[..]);

        // The displayed frame is still the root: renderData is the base.
        assert_eq!(&[255u8, 0, 0, 255][..], img.render_data().bytes().unwrap());

        // Frame bytes count against the storage total.
        assert_eq!(8usize, storage(&t).total_bytes);
    }

    #[test]
    fn kittygfx_animation_frame_gap_normalization_on_create() {
        let mut t = init(5, 5);
        assert!(exec(&mut t, "a=t,f=24,s=1,v=1,i=1;/wAA").unwrap().ok());

        for input in [
            "a=f,i=1,f=24,s=1,v=1,z=100;AAD/",
            "a=f,i=1,f=24,s=1,v=1,z=-5;AAD/",
        ] {
            assert!(exec(&mut t, input).unwrap().ok());
        }

        let anim = anim_of(&mut t);
        assert_eq!(100u32, anim.frames[0].gap_ms);
        assert_eq!(0u32, anim.frames[1].gap_ms);
    }

    #[test]
    fn kittygfx_animation_background_fill_and_offset_composition() {
        let mut t = init(5, 5);

        // 2x1 RGB white image ("////////" is FFFFFF FFFFFF).
        assert!(exec(&mut t, "a=t,f=24,s=2,v=1,i=1;////////").unwrap().ok());

        // New frame: 1x1 RGB white transmitted at x=1 over an opaque red
        // background (Y=4278190335 is 0xff0000ff, R in the MSB).
        assert!(exec(&mut t, "a=f,i=1,f=24,s=1,v=1,x=1,Y=4278190335;////")
            .unwrap()
            .ok());

        let anim = anim_of(&mut t);
        assert_eq!(
            &[255u8, 0, 0, 255, 255, 255, 255, 255][..],
            &anim.frames[0].data[..]
        );
    }

    #[test]
    fn kittygfx_animation_create_from_base_frame_with_overwrite() {
        let mut t = init(5, 5);
        assert!(exec(&mut t, "a=t,f=24,s=1,v=1,i=1;/wAA").unwrap().ok());

        // Frame 2 based on the root (c=1) with a semi-transparent blue
        // pixel ("AAD/gA==" is 0000FF80) in overwrite mode: the source
        // replaces the canvas including alpha.
        let resp = exec(&mut t, "a=f,i=1,f=32,s=1,v=1,c=1,X=1;AAD/gA==").unwrap();
        assert!(resp.ok());

        let anim = anim_of(&mut t);
        assert_eq!(&[0u8, 0, 255, 128][..], &anim.frames[0].data[..]);
    }

    #[test]
    fn kittygfx_animation_alpha_blend_composes_over_base_frame() {
        let mut t = init(5, 5);
        assert!(exec(&mut t, "a=t,f=24,s=1,v=1,i=1;/wAA").unwrap().ok());

        // Blend a fully opaque blue pixel over the root: alpha blending
        // an opaque source is equivalent to a copy.
        assert!(exec(&mut t, "a=f,i=1,f=32,s=1,v=1,c=1;AAD//w==").unwrap().ok());

        // Blend a fully transparent pixel over the root: the canvas is
        // unchanged (still the red base).
        assert!(exec(&mut t, "a=f,i=1,f=32,s=1,v=1,c=1;AAD/AA==").unwrap().ok());

        let anim = anim_of(&mut t);
        assert_eq!(&[0u8, 0, 255, 255][..], &anim.frames[0].data[..]);
        assert_eq!(&[255u8, 0, 0, 255][..], &anim.frames[1].data[..]);
    }

    #[test]
    fn kittygfx_animation_edit_root_frame_bumps_generation() {
        let mut t = init(5, 5);
        assert!(exec(&mut t, "a=t,f=24,s=1,v=1,i=1;/wAA").unwrap().ok());
        let gen1 = storage_mut(&mut t)
            .image_ptr_by_id_or_number(1, 0)
            .unwrap()
            .generation;

        // r=1 edits the root frame in place. The root is the displayed
        // frame so the image generation must change.
        let resp = exec(&mut t, "a=f,i=1,f=24,s=1,v=1,r=1;AAD/").unwrap();
        assert!(resp.ok());
        assert_eq!(1u32, resp.frame);

        let img = storage_mut(&mut t).image_ptr_by_id_or_number(1, 0).unwrap();
        assert_eq!(&[0u8, 0, 255, 255][..], img.data.bytes().unwrap());
        assert!(img.generation > gen1);
    }

    #[test]
    fn kittygfx_animation_edit_frame_gap_without_data_change() {
        let mut t = init(5, 5);
        assert!(exec(&mut t, "a=t,f=24,s=1,v=1,i=1;/wAA").unwrap().ok());

        // Create frame 2, then edit it with z=-1: gap becomes gapless.
        // Editing with z=0 leaves the gap unchanged (no 40ms default).
        assert!(exec(&mut t, "a=f,i=1,f=24,s=1,v=1,z=77;AAD/").unwrap().ok());
        assert!(exec(&mut t, "a=f,i=1,f=24,s=1,v=1,r=2;/wAA").unwrap().ok());
        assert_eq!(77u32, anim_of(&mut t).frames[0].gap_ms);

        assert!(exec(&mut t, "a=f,i=1,f=24,s=1,v=1,r=2,z=-1;/wAA").unwrap().ok());
        assert_eq!(0u32, anim_of(&mut t).frames[0].gap_ms);
    }

    #[test]
    fn kittygfx_animation_frame_errors() {
        let mut t = init(5, 5);
        assert!(exec(&mut t, "a=t,f=24,s=1,v=1,i=1;/wAA").unwrap().ok());

        let cases: [(&str, &str); 4] = [
            // Unknown image.
            ("a=f,i=9,f=24,s=1,v=1;AAD/", "ENOENT: image not found"),
            // Frame bigger than the image.
            ("a=f,i=1,f=24,s=2,v=1;////////", "EINVAL: frame dimensions exceed image"),
            // Nonexistent base frame.
            ("a=f,i=1,f=24,s=1,v=1,c=5;AAD/", "EINVAL: base frame not found"),
            // Insufficient data for the declared rectangle.
            ("a=f,i=1,f=24,s=1,v=1;AA==", "ENODATA: insufficient data"),
        ];
        for (input, message) in cases {
            let resp = exec(&mut t, input).unwrap();
            assert!(!resp.ok());
            assert_eq!(message, resp.message);
        }

        // None of the failures may have created a frame.
        let img = storage_mut(&mut t).image_ptr_by_id_or_number(1, 0).unwrap();
        if let Some(anim) = &img.animation {
            assert_eq!(0, anim.frames.len());
        }
    }

    #[test]
    fn kittygfx_animation_excess_frame_data_is_truncated() {
        let mut t = init(5, 5);
        assert!(exec(&mut t, "a=t,f=24,s=1,v=1,i=1;/wAA").unwrap().ok());

        // Six bytes for a 1x1 RGB frame: Kitty tolerates the excess.
        assert!(exec(&mut t, "a=f,i=1,f=24,s=1,v=1;AAD/////").unwrap().ok());

        let anim = anim_of(&mut t);
        assert_eq!(&[0u8, 0, 255, 255][..], &anim.frames[0].data[..]);
    }

    #[test]
    fn kittygfx_animation_chunked_frame_transmission() {
        let mut t = init(5, 5);

        // 2x1 RGB white image.
        assert!(exec(&mut t, "a=t,f=24,s=2,v=1,i=1;////////").unwrap().ok());

        // Transmit a 2x1 frame in two chunks of one pixel each. The
        // chunks repeat a=f per the protocol. No response until the
        // final chunk.
        assert!(exec(&mut t, "a=f,i=1,f=24,s=2,v=1,z=60,m=1;/wAA").is_none());
        assert!(storage(&t).loading.is_some());
        {
            let resp = exec(&mut t, "a=f,m=0;AAD/").unwrap();
            assert!(resp.ok());
            assert_eq!(1u32, resp.id);
            assert_eq!(2u32, resp.frame);
        }
        assert!(storage(&t).loading.is_none());

        let anim = anim_of(&mut t);
        assert_eq!(60u32, anim.frames[0].gap_ms);
        assert_eq!(
            &[255u8, 0, 0, 255, 0, 0, 255, 255][..],
            &anim.frames[0].data[..]
        );
    }

    #[test]
    fn kittygfx_animation_chunked_frame_continuation_without_a_f() {
        let mut t = init(5, 5);

        assert!(exec(&mut t, "a=t,f=24,s=2,v=1,i=1;////////").unwrap().ok());

        // The spec requires chunks to repeat a=f, but a bare continuation
        // (which parses as a transmit action) must still continue the
        // frame load rather than turn into an image transmission.
        assert!(exec(&mut t, "a=f,i=1,f=24,s=2,v=1,m=1;/wAA").is_none());
        {
            let resp = exec(&mut t, "m=0;AAD/").unwrap();
            assert!(resp.ok());
            assert_eq!(2u32, resp.frame);
        }

        assert_eq!(1, anim_of(&mut t).frames.len());
    }

    #[test]
    fn kittygfx_animation_non_direct_transmission_abandons_chunked_frame_load() {
        let mut t = init(5, 5);

        // Allow file (t=f) transmissions and write a 2x1 green RGB frame.
        storage_mut(&mut t).image_limits.file = true;
        let tmp_dir = TmpDir::new();
        let payload = test_file_payload(&tmp_dir, &[0, 255, 0, 0, 255, 0]);

        // 2x1 RGB white image.
        assert!(exec(&mut t, "a=t,f=24,s=2,v=1,i=1;////////").unwrap().ok());

        // Begin a chunked frame load.
        assert!(exec(&mut t, "a=f,i=1,f=24,s=2,v=1,m=1;/wAA").is_none());
        assert!(storage(&t).loading.is_some());

        // A file transmission can't be a continuation chunk, so it abandons
        // the chunked frame and loads a new frame from the file instead.
        {
            let str = format!("a=f,t=f,i=1,f=24,s=2,v=1;{payload}");
            let resp = exec(&mut t, &str).unwrap();
            assert!(resp.ok());
            assert_eq!(2u32, resp.frame);
        }
        assert!(storage(&t).loading.is_none());

        let anim = anim_of(&mut t);
        assert_eq!(1, anim.frames.len());
        assert_eq!(
            &[0u8, 255, 0, 255, 0, 255, 0, 255][..],
            &anim.frames[0].data[..]
        );
    }

    #[test]
    fn kittygfx_animation_control_command_sets_gap_current_state_and_loops() {
        let mut t = init(5, 5);
        assert!(exec(&mut t, "a=t,f=24,s=1,v=1,i=1;/wAA").unwrap().ok());
        assert!(exec(&mut t, "a=f,i=1,f=24,s=1,v=1;AAD/").unwrap().ok());
        let gen1 = storage_mut(&mut t)
            .image_ptr_by_id_or_number(1, 0)
            .unwrap()
            .generation;

        // One command can set several things: the root frame's gap
        // (r=1,z=50), the current frame (c=2), the state (s=3, running),
        // and the loop count (v=3, meaning two loops). Successful a=a
        // commands never respond.
        assert!(exec(&mut t, "a=a,i=1,r=1,z=50,c=2,s=3,v=3").is_none());

        let img = storage_mut(&mut t).image_ptr_by_id_or_number(1, 0).unwrap();
        let anim = img.animation.as_ref().unwrap();
        assert_eq!(50u32, anim.root_gap_ms);
        assert_eq!(1u32, anim.current_index);
        assert_eq!(animation::State::Running, anim.state);
        assert_eq!(2u32, anim.max_loops);

        // Changing the current frame changes the displayed content.
        assert!(img.generation > gen1);
        assert_eq!(&[0u8, 0, 255, 255][..], img.render_data().bytes().unwrap());
    }

    #[test]
    fn kittygfx_animation_control_command_ignores_invalid_values() {
        let mut t = init(5, 5);
        assert!(exec(&mut t, "a=t,f=24,s=1,v=1,i=1;/wAA").unwrap().ok());

        // Out-of-range frame numbers and s values are silently ignored.
        assert!(exec(&mut t, "a=a,i=1,r=9,z=50,c=9,s=7").is_none());

        let anim = anim_of(&mut t);
        assert_eq!(0u32, anim.root_gap_ms);
        assert_eq!(0u32, anim.current_index);
        assert_eq!(animation::State::Stopped, anim.state);
    }

    #[test]
    fn kittygfx_animation_control_command_missing_image_responds_enoent() {
        let mut t = init(5, 5);

        let resp = exec(&mut t, "a=a,i=9,s=3").unwrap();
        assert!(!resp.ok());
        assert_eq!("ENOENT: image not found", resp.message);
    }

    #[test]
    fn kittygfx_animation_compose_frames() {
        let mut t = init(5, 5);

        // 2x1 RGB image: red then blue ("/wAAAAD/" is FF0000 0000FF).
        assert!(exec(&mut t, "a=t,f=24,s=2,v=1,i=1;/wAAAAD/").unwrap().ok());
        // Frame 2 starts as a transparent canvas with a white pixel at
        // x=0 ("////" is FFFFFF).
        assert!(exec(&mut t, "a=f,i=1,f=24,s=1,v=1;////").unwrap().ok());

        // Compose the root's pixel at (1,0) onto frame 2 at (0,0):
        // r=1 is the source frame, c=2 the destination, X/Y the source
        // offsets, x/y the destination offsets.
        {
            let resp = exec(&mut t, "a=c,i=1,r=1,c=2,w=1,h=1,X=1,x=0").unwrap();
            assert!(resp.ok());
            assert_eq!(0u32, resp.frame);
        }

        let anim = anim_of(&mut t);
        assert_eq!(&[0u8, 0, 255, 255, 0, 0, 0, 0][..], &anim.frames[0].data[..]);
    }

    #[test]
    fn kittygfx_animation_compose_current_frame_bumps_generation() {
        let mut t = init(5, 5);

        // 2x1 RGB white image; the root frame is the displayed frame.
        assert!(exec(&mut t, "a=t,f=24,s=2,v=1,i=1;////////").unwrap().ok());
        let gen1 = storage_mut(&mut t)
            .image_ptr_by_id_or_number(1, 0)
            .unwrap()
            .generation;

        // a=c works on images without animation state: both frames are
        // the root. Copy pixel 0 onto pixel 1.
        assert!(exec(&mut t, "a=c,i=1,r=1,c=1,w=1,h=1,x=1").unwrap().ok());
        let img = storage_mut(&mut t).image_ptr_by_id_or_number(1, 0).unwrap();
        assert!(img.generation > gen1);
    }

    #[test]
    fn kittygfx_animation_compose_errors() {
        let mut t = init(5, 5);
        assert!(exec(&mut t, "a=t,f=24,s=1,v=1,i=1;/wAA").unwrap().ok());

        let cases: [(&str, &str); 6] = [
            ("a=c,i=9,r=1,c=1", "ENOENT: image not found"),
            ("a=c,i=1,r=2,c=1", "ENOENT: source frame not found"),
            ("a=c,i=1,r=1,c=2", "ENOENT: destination frame not found"),
            // 1x1 image: any offset pushes the rect out of bounds.
            ("a=c,i=1,r=1,c=1,x=1", "EINVAL: destination rectangle out of bounds"),
            ("a=c,i=1,r=1,c=1,X=1", "EINVAL: source rectangle out of bounds"),
            // Same frame, same (whole-image) rect: overlap.
            ("a=c,i=1,r=1,c=1", "EINVAL: source and destination rectangles overlap"),
        ];
        for (input, message) in cases {
            let resp = exec(&mut t, input).unwrap();
            assert!(!resp.ok());
            assert_eq!(message, resp.message);
        }
    }

    #[test]
    fn kittygfx_animation_delete_frame_promotes_root_and_fixes_current() {
        let mut t = init(5, 5);
        assert!(exec(&mut t, "a=t,f=24,s=1,v=1,i=1;/wAA").unwrap().ok());

        // Frames 2 and 3: blue and white.
        for input in [
            "a=f,i=1,f=24,s=1,v=1,z=10;AAD/",
            "a=f,i=1,f=24,s=1,v=1,z=20;////",
        ] {
            assert!(exec(&mut t, input).unwrap().ok());
        }
        let total_before = storage(&t).total_bytes;

        // Delete the root frame (r omitted selects it): frame 2 becomes
        // the new root, including its gap. Deletes never respond.
        assert!(exec(&mut t, "a=d,d=f,i=1").is_none());
        {
            let img = storage_mut(&mut t).image_ptr_by_id_or_number(1, 0).unwrap();
            let anim = img.animation.as_ref().unwrap();
            assert_eq!(2u32, anim.frame_count());
            assert_eq!(10u32, anim.root_gap_ms);
            assert_eq!(&[0u8, 0, 255, 255][..], img.data.bytes().unwrap());
        }
        assert_eq!(total_before - 4, storage(&t).total_bytes);

        // Delete a frame number past the end: clamps to the last frame.
        assert!(exec(&mut t, "a=d,d=f,i=1,r=9").is_none());
        assert_eq!(1u32, anim_of(&mut t).frame_count());
        assert_eq!(total_before - 8, storage(&t).total_bytes);

        // Down to one frame: d=f is now a no-op and the image survives.
        assert!(exec(&mut t, "a=d,d=f,i=1").is_none());
        assert!(storage(&t).image_by_id(1).is_some());
    }

    #[test]
    fn kittygfx_animation_uppercase_frame_delete_removes_non_animated_image() {
        let mut t = init(5, 5);

        // Transmit and place: unlike d=I, an uppercase frame delete on a
        // non-animated image removes it even though it is placed.
        assert!(exec(&mut t, "a=T,f=24,s=1,v=1,i=1,C=1;/wAA").unwrap().ok());
        assert_eq!(1, storage(&t).placements.len());

        assert!(exec(&mut t, "a=d,d=F,i=1").is_none());
        assert!(storage(&t).image_by_id(1).is_none());
        assert_eq!(0, storage(&t).placements.len());
        assert_eq!(0usize, storage(&t).total_bytes);
    }

    #[test]
    fn kittygfx_animation_deleting_current_frame_refreshes_display() {
        let mut t = init(5, 5);
        assert!(exec(&mut t, "a=t,f=24,s=1,v=1,i=1;/wAA").unwrap().ok());
        for input in [
            "a=f,i=1,f=24,s=1,v=1;AAD/",
            "a=f,i=1,f=24,s=1,v=1;////",
            "a=a,i=1,c=3",
        ] {
            let _ = exec(&mut t, input);
        }

        assert_eq!(2u32, anim_of(&mut t).current_index);
        let gen1 = storage_mut(&mut t)
            .image_ptr_by_id_or_number(1, 0)
            .unwrap()
            .generation;

        // Deleting the last frame (the current one) clamps the index and
        // refreshes the display.
        assert!(exec(&mut t, "a=d,d=f,i=1,r=3").is_none());
        let img = storage_mut(&mut t).image_ptr_by_id_or_number(1, 0).unwrap();
        assert_eq!(1u32, img.animation.as_ref().unwrap().current_index);
        assert!(img.generation > gen1);
        assert_eq!(&[0u8, 0, 255, 255][..], img.render_data().bytes().unwrap());
    }

    #[test]
    fn kittygfx_animation_retransmitting_base_image_resets_animation() {
        let mut t = init(5, 5);
        assert!(exec(&mut t, "a=t,f=24,s=1,v=1,i=1;/wAA").unwrap().ok());
        assert!(exec(&mut t, "a=f,i=1,f=24,s=1,v=1;AAD/").unwrap().ok());
        assert_eq!(8usize, storage(&t).total_bytes);

        // Retransmit the base image: the animation is dropped and the
        // frame bytes are credited back.
        assert!(exec(&mut t, "a=t,f=24,s=1,v=1,i=1;/wAA").unwrap().ok());
        let img = storage_mut(&mut t).image_ptr_by_id_or_number(1, 0).unwrap();
        assert!(img.animation.is_none());
        assert_eq!(3usize, storage(&t).total_bytes);
    }

    #[test]
    fn kittygfx_animation_control_negative_gap_makes_frame_gapless() {
        let mut t = init(5, 5);
        assert!(exec(&mut t, "a=t,f=24,s=1,v=1,i=1;/wAA").unwrap().ok());
        assert!(exec(&mut t, "a=f,i=1,f=24,s=1,v=1;AAD/").unwrap().ok());

        assert!(exec(&mut t, "a=a,i=1,r=2,z=-1").is_none());

        assert_eq!(0u32, anim_of(&mut t).frames[0].gap_ms);
    }
}
