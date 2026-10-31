//! Port of `src/terminal/snapshot/kitty.zig`.
//!
//! STATUS: ported
//!
//! Kitty graphics records: the images, pixels, placements and in-progress
//! chunked load of each screen.
//!
//! A terminal's Kitty graphics live in one image storage per screen. These
//! records carry that storage through a snapshot, so a restored terminal
//! shows the same images in the same places:
//!
//! - KITTY_IMAGE_STORAGE holds one screen's images, its placements on the
//!   pages of its SCREEN sequence, and its in-progress chunked load, if any.
//! - KITTY_IMAGE_PAYLOAD holds the pixels of one image or one animation frame.
//! - KITTY_IMAGE_PAGE_PLACEMENTS holds the placements pinned to one history
//!   page.
//!
//! All of them are optional records. A decoder that does not know them,
//! including any build without Kitty graphics, skips them (see "Unknown
//! Records" in `record.zig`) and restores the same terminal without its
//! images. They are only written when there is something in them, so a
//! terminal without images encodes exactly as it did before these records
//! existed.
//!
//! ## Restoring a Terminal with Images
//!
//! Nothing changes for a caller that uses `snapshot.decode`: the restored
//! terminal has its images, pixels and placements, ready to draw.
//!
//! With `snapshot.Decoder`, the terminal handed back at READY already has
//! every image and placement, but the pixels have not arrived yet. Each
//! image is "pending": it has an ID, a size and places on screen, so the
//! layout is complete, but there is nothing to draw for it. The pixels
//! arrive through `next`, one image or frame per call, mixed in with the
//! history pages. A renderer that checks `Image.renderData` draws an image
//! as soon as its pixels are in, without any other coordination:
//!
//! ```ignore
//! let mut decoded = decoder.ready(&options)?;
//! let mut terminal = decoded.to_owned();
//!
//! // Pending images take part in layout, just not in drawing.
//! let storage = &terminal.screens.active.kitty_images;
//! storage.image_by_id(1).unwrap().render_data().is_pending(); // true: nothing to draw yet
//!
//! // Each call restores one history page or one image's pixels.
//! while let Some(progress) = decoder.next(&mut terminal)? {
//!     match progress {
//!         Progress::Image(p) if p.image_id == 1 => {
//!             // render_data().is_pending() is now false: draw it
//!         }
//!         _ => {}
//!     }
//! }
//! ```
//!
//! `Decoded.kitty` reports how many images were restored and how many
//! bytes of pixels are still to come, which is enough for a progress bar.
//!
//! ## Where the Records Go
//!
//! ```text
//! +--------------------------------+
//! | SCREEN + PAGE records          |
//! | KITTY_IMAGE_STORAGE (optional) |  after each screen's records
//! +--------------------------------+
//! | CONTINUATION, READY            |
//! +--------------------------------+
//! | KITTY_IMAGE_PAYLOAD (optional) |  per image or frame, anywhere
//! | ...                            |  from here to FINISH
//! +--------------------------------+
//! | HISTORY                        |  per screen
//! | PAGE                           |
//! | KITTY_IMAGE_PAGE_PLACEMENTS    |  optional, after a PAGE with placements
//! | ...                            |
//! +--------------------------------+
//! | FINISH                         |
//! +--------------------------------+
//! ```
//!
//! KITTY_IMAGE_STORAGE comes before READY, so the terminal handed back at
//! READY already knows every image and where it is placed. A placement is
//! pinned to a cell of some page, and where its record goes depends on that
//! page:
//!
//! - Pinned to a page of the SCREEN sequence: KITTY_IMAGE_STORAGE, which
//!   names the page by its position in that sequence.
//! - Pinned to a history page: the KITTY_IMAGE_PAGE_PLACEMENTS record right
//!   after that PAGE. If the decoder has to drop the page, it drops these
//!   placements with it.
//! - Virtual placements, which are drawn through placeholder characters and
//!   have no pin: KITTY_IMAGE_STORAGE.
//! - Relative placements, which are positioned relative to a parent
//!   placement: the same record as their parent, after it. A parent can be
//!   relative itself, so this follows the parents up to the first one that
//!   is pinned or virtual, and uses its record.
//!
//! ## Images
//!
//! KITTY_IMAGE_STORAGE describes each image, including its size, format and
//! any animation, but not its pixels. The decoder adds each image to storage
//! as a pending image (see `ImageStorage.PendingImage`), which reserves the
//! image's full size against the decoder's storage limit. Animation frames
//! are restored pending in the same way. A pending image can be placed and
//! queried like any other image. It just isn't drawn yet.
//!
//! Images are written oldest first. The decoder adds them in that order, so
//! when the decoder's limit is smaller than the encoder's, older images are
//! evicted to make room for newer ones, exactly as live transmissions would
//! evict them.
//!
//! Image generations are never written. They are only meaningful within
//! one process, so the decoder stamps fresh ones as it adds each image.
//!
//! ## Pixels
//!
//! The pixels come after READY, one KITTY_IMAGE_PAYLOAD record per image or
//! frame, so a restored terminal can be shown before they arrive. Each
//! payload names its screen and image, and completes that pending image or
//! frame. An animation does not play until all of its frames have arrived.
//!
//! A payload may come anywhere after READY and before FINISH, in any order,
//! with one exception: it never comes between a PAGE and that page's
//! KITTY_IMAGE_PAGE_PLACEMENTS, which must follow the page directly. This
//! leaves encoders free to choose the order. One could send each image's
//! pixels right after the history page that first shows it, for example, so
//! that large images deep in scrollback don't hold up recent history.
//!
//! This encoder writes payloads where the reader needs them soonest:
//!
//! 1. Right after READY, the images placed in the viewport of the screen
//!    that is shown, so what is on screen can be drawn first. For each
//!    image, the frame it shows comes before its other frames.
//! 2. Right after each history page's KITTY_IMAGE_PAGE_PLACEMENTS, the
//!    images first placed on that page, so an image in scrollback arrives
//!    with the text around it, and a large one deep in scrollback doesn't
//!    hold up the recent history before it.
//! 3. After all history, everything else: images in the viewport of a
//!    screen that is not shown, then images that are not placed where the
//!    reader can see them. Nothing waits on these.
//!
//! Live input may delete an image before its payload arrives, or send a
//! new image with the same ID. The payload is then dropped rather than
//! attached to whatever image now has that ID.
//!
//! Payloads carry the pixels as they are, with no compression of their
//! own. A snapshot is normally compressed on its way to the reader, and on
//! real images that compressor does at least as well on raw pixels as it
//! would on pixels that were compressed first. Compressing them here would
//! only add encoder time, and everything after the pixels would wait for
//! it.
//!
//! The in-progress load is the one place where image data comes before
//! READY. It carries every byte received so far, because the next chunk
//! from live input is appended to them. A snapshot taken partway through a
//! large transmission can therefore hold megabytes of image data in
//! KITTY_IMAGE_STORAGE. Snapshots taken mid-transmission are rare, so this
//! cost is accepted.
//!
//! All integers are little-endian.
//!
//! ## Binary Format
//!
//! ### KITTY_IMAGE_STORAGE
//!
//! ```text
//!   0 +------------------------------------+
//!     | Screen key (u16)                   |
//!   2 +------------------------------------+
//!     | Next image ID (u32)                |
//!   6 +------------------------------------+
//!     | Next internal placement ID (u32)   |
//!  10 +------------------------------------+
//!     | Image count (u32)                  |
//!  14 +------------------------------------+
//!     | Placement count (u32)              |
//!  18 +------------------------------------+
//!     | In-progress load present (u8)      |
//!  19 +------------------------------------+
//!     | Image * image count                |
//!     +------------------------------------+
//!     | Placement * placement count        |
//!     +------------------------------------+
//!     | In-progress load, when present     |
//! end +------------------------------------+
//! ```
//!
//! Images are ordered oldest first. Placements are ordered so every parent
//! precedes its relative children.
//!
//! #### Image
//!
//! ```text
//!   0 +------------------------------------+
//!     | ID (u32)                           |
//!   4 +------------------------------------+
//!     | Number (u32)                       |
//!   8 +------------------------------------+
//!     | Width (u32)                        |
//!  12 +------------------------------------+
//!     | Height (u32)                       |
//!  16 +------------------------------------+
//!     | Format (u8)                        |
//!  17 +------------------------------------+
//!     | Flags (u8)                         |
//!  18 +------------------------------------+
//!     | Decoded byte length (u64)          |
//!  26 +------------------------------------+
//!     | Animation present (u8)             |
//!  27 +------------------------------------+
//!     | Animation, when present            |
//! end +------------------------------------+
//! ```
//!
//! Formats are 0 rgb, 1 rgba, 2 png, 3 gray_alpha and 4 gray. Stored images
//! are never png. Flag bit 0 is the transient usage hint, and bit 1 marks
//! an automatically assigned ID. The other bits are zero. The decoded length
//! is width * height * bytes per pixel.
//!
//! #### Animation
//!
//! ```text
//!   0 +------------------------------------+
//!     | State (u8)                         |
//!   1 +------------------------------------+
//!     | Max loops (u32)                    |
//!   5 +------------------------------------+
//!     | Current loop (u32)                 |
//!   9 +------------------------------------+
//!     | Current frame index (u32)          |
//!  13 +------------------------------------+
//!     | Root frame gap ms (u32)            |
//!  17 +------------------------------------+
//!     | Frame count (u32)                  |
//!  21 +------------------------------------+
//!     | Frame * frame count                |
//! end +------------------------------------+
//!
//! Frame:
//!   0 +------------------------------------+
//!     | Gap ms (u32)                       |
//!   4 +------------------------------------+
//!     | Decoded byte length (u64)          |
//!  12 +------------------------------------+
//! ```
//!
//! States are 0 stopped, 1 loading and 2 running. The frame count excludes
//! the root frame, which is the image itself. A frame's decoded length is
//! width * height * 4. The time the current frame was shown is not encoded;
//! playback re-anchors on the decoder's clock.
//!
//! #### Placement
//!
//! ```text
//!   0 +------------------------------------+
//!     | Image ID (u32)                     |
//!   4 +------------------------------------+
//!     | Placement ID tag (u8)              |
//!   5 +------------------------------------+
//!     | Placement ID (u32)                 |
//!   9 +------------------------------------+
//!     | Location kind (u8)                 |
//!  10 +------------------------------------+
//!     | Location                           |
//!     +------------------------------------+
//!     | X offset, Y offset (u32 * 2)       |
//!     +------------------------------------+
//!     | Source x, y, width, height         |
//!     | (u32 * 4)                          |
//!     +------------------------------------+
//!     | Columns, rows (u32 * 2)            |
//!     +------------------------------------+
//!     | Z (i32)                            |
//! end +------------------------------------+
//! ```
//!
//! Placement ID tags are 0 internal and 1 external. Location kinds and their
//! locations are:
//!
//! - 0 pin: in KITTY_IMAGE_STORAGE, page index within the screen's SCREEN
//!   sequence (u16), then y (u16) and x (u16) within that page. In
//!   KITTY_IMAGE_PAGE_PLACEMENTS, only y (u16) and x (u16) within the
//!   preceding history page.
//! - 1 virtual: no location bytes. KITTY_IMAGE_STORAGE only.
//! - 2 relative: parent image ID (u32), parent placement ID tag (u8), parent
//!   placement ID (u32), horizontal offset (i32), vertical offset (i32).
//!
//! #### In-progress load
//!
//! A chunked transmission that was cut off between two chunks. The decoder
//! installs it, so the next chunk from live input continues it.
//!
//! ```text
//! +------------------------------------+
//! | ID, number, width, height (u32 * 4)|
//! | Format (u8)                        |
//! | Compression (u8)                   |
//! | Flags (u8)                         |
//! | Quiet (u8)                         |
//! | Response ID, image number,         |
//! | placement ID, frame (u32 * 4)      |
//! | Display present (u8)               |
//! | Display, when present              |
//! | Frame present (u8)                 |
//! | Frame, when present                |
//! | Received byte count (u32)          |
//! | Received bytes                     |
//! +------------------------------------+
//! ```
//!
//! Format and flags are as for images, except that a load may be png.
//! Compression is 0 none or 1 zlib. Quiet is 0 no, 1 ok or 2 failures. The
//! received bytes are the chunk data received so far, before any
//! decompression or png decoding.
//!
//! Display is the placement to create once the load completes:
//!
//! - Image ID, image number, placement ID, x, y, width, height, x offset,
//!   y offset, columns and rows (u32 each).
//! - Cursor movement (u8): 0 after, 1 none.
//! - Virtual (u8): 1 for a virtual placement.
//! - Parent ID and parent placement ID (u32 each).
//! - Horizontal offset, vertical offset and z (i32 each).
//!
//! Frame is present when the load is an animation frame rather than a new
//! image:
//!
//! - Format (u8), as for the load.
//! - Medium (u8): 0 direct, 1 file, 2 temporary file, 3 shared memory.
//! - Width, height, size, offset, image ID, image number and placement ID
//!   (u32 each).
//! - Compression (u8), as for the load.
//! - More chunks (u8).
//! - Usage flags (u32).
//! - X, y, base frame and edited frame (u32 each).
//! - Gap in milliseconds (i32).
//! - Composition mode (u8): 0 alpha blend, 1 overwrite.
//! - Background color (u32).
//!
//! ### KITTY_IMAGE_PAYLOAD
//!
//! ```text
//!   0 +------------------------------------+
//!     | Screen key (u16)                   |
//!   2 +------------------------------------+
//!     | Image ID (u32)                     |
//!   6 +------------------------------------+
//!     | Frame number (u32)                 |
//!  10 +------------------------------------+
//!     | Byte length (u64)                  |
//!  18 +------------------------------------+
//!     | Pixels                             |
//! end +------------------------------------+
//! ```
//!
//! Frames are numbered as the Kitty graphics protocol numbers them. Frame 1
//! is the image itself, so its byte length is the image's decoded length.
//! Frames 2 and up are animation frames, whose byte length is
//! width * height * 4. The pixels are exactly that many bytes, and the
//! record ends with them.
//!
//! Notes on the port:
//! - `std.Io`/allocator parameters are dropped. Allocation failure aborts
//!   in Rust, so `ScreenPlan::init` returns `Option` instead of
//!   `Allocator.Error!?ScreenPlan`.
//! - `PlannedPlacement` holds a copy of its `Placement` (which is `Copy`)
//!   instead of a pointer into storage, and `PlannedPayload` borrows its
//!   image for the plan's lifetime.
//! - `PayloadTarget`/`PageTarget` hold `&mut Screen` (Zig: `*Screen`), and
//!   `PayloadTargets` is a `[Option<PayloadTarget>; Key::COUNT]` array
//!   indexed by the key (Zig: `std.EnumMap`).
//! - `Plan` is always `ScreenPlan` because Kitty graphics are enabled in
//!   every Rust build (Zig: `void` without Kitty graphics).

use std::collections::HashMap;

use super::io::{self, Error};
use super::record;
use crate::terminal::kitty::graphics_animation::{self as animation, Animation};
use crate::terminal::kitty::graphics_command as graphics_command;
use crate::terminal::kitty::graphics_image::{Data, FrameContext, Image, LoadingImage, Metadata, TemporaryFile};
use crate::terminal::kitty::graphics_storage::{
    ImageStorage, Location as PlacementLocation, PendingImage, Placement, PlacementId, PlacementKey, PlacementTag,
    Relative,
};
use crate::terminal::page_list::{Node, NodePtr, Pin};
use crate::terminal::point;
use crate::terminal::screen::Screen as TerminalScreen;
use crate::terminal::screen_set::Key as TerminalScreenKey;

const LOG_TARGET: &str = "snapshot_kitty";

/// Errors possible while encoding Kitty graphics records: allocation and
/// `record::Writer` finish errors.
pub type EncodeError = Error;

/// Errors possible while decoding Kitty graphics records: allocation, read
/// and record finish errors, plus:
///
/// - `UnexpectedRecordTag`: the record is valid but not the expected kind.
/// - `UnexpectedKittyKey`: the record names an unknown screen, or a
///   different screen from the one whose records it follows.
pub type DecodeError = Error;

#[inline(always)]
fn nd<'a>(node: NodePtr) -> &'a Node {
    // SAFETY: node pointers come from the live PageList being walked.
    unsafe { &*node.as_ptr() }
}

/// Read and validate a screen key.
fn decode_key(reader: &mut dyn io::Reader) -> Result<TerminalScreenKey, DecodeError> {
    let raw = io::read_int::<u16>(reader)?;
    TerminalScreenKey::ALL
        .get(raw as usize)
        .copied()
        .ok_or(Error::UnexpectedKittyKey)
}

fn encode_key(writer: &mut dyn io::Writer, key: TerminalScreenKey) -> io::Result<()> {
    io::write_int::<u16>(writer, key as u16)
}

fn write_i32(writer: &mut dyn io::Writer, value: i32) -> io::Result<()> {
    io::write_int::<u32>(writer, value as u32)
}

fn read_i32(reader: &mut dyn io::Reader) -> io::Result<i32> {
    Ok(io::read_int::<u32>(reader)? as i32)
}

/// Image pixel formats on the wire.
type Format = graphics_command::Format;

fn encode_format(format: Format) -> u8 {
    match format {
        Format::Rgb => 0,
        Format::Rgba => 1,
        Format::Png => 2,
        Format::GrayAlpha => 3,
        Format::Gray => 4,
    }
}

fn decode_format(raw: u8) -> Option<Format> {
    match raw {
        0 => Some(Format::Rgb),
        1 => Some(Format::Rgba),
        2 => Some(Format::Png),
        3 => Some(Format::GrayAlpha),
        4 => Some(Format::Gray),
        _ => None,
    }
}

/// Image flags shared by images and the in-progress load
/// (Zig: `packed struct(u8)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct ImageFlags {
    transient: bool,
    implicit_id: bool,
    /// The remaining 6 bits.
    _padding: u8,
}

impl ImageFlags {
    fn to_bits(self) -> u8 {
        (self.transient as u8) | ((self.implicit_id as u8) << 1) | (self._padding << 2)
    }

    fn from_bits(raw: u8) -> ImageFlags {
        ImageFlags {
            transient: raw & 1 != 0,
            implicit_id: raw & 2 != 0,
            _padding: raw >> 2,
        }
    }
}

fn image_flags(img: &Image) -> ImageFlags {
    ImageFlags {
        transient: img.metadata.transient,
        implicit_id: img.metadata.implicit_id,
        _padding: 0,
    }
}

/// The decoded byte length of an image, or null when its dimensions or
/// format cannot describe stored pixels.
fn image_len(width: u32, height: u32, format: Format) -> Option<usize> {
    if width == 0 || height == 0 {
        return None;
    }
    if format == Format::Png {
        return None;
    }
    let bpp = graphics_command::Transmission::format_bpp(format) as usize;
    (width as usize * height as usize).checked_mul(bpp)
}

/// Turn a Zig-style `lessThan` into an `Ordering` for sorting.
fn order_by<T>(less_than: impl Fn(&T, &T) -> bool) -> impl Fn(&T, &T) -> std::cmp::Ordering {
    move |lhs, rhs| {
        if less_than(lhs, rhs) {
            std::cmp::Ordering::Less
        } else if less_than(rhs, lhs) {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Equal
        }
    }
}

// ---------------------------------------------------------------------------
// Encoding

/// One placement, together with its root: the placement that decides
/// where it is encoded. A pinned or virtual placement is its own root. A
/// relative placement's root is the first placement up its parent chain
/// that is pinned or virtual.
#[derive(Debug, Clone, Copy)]
struct PlannedPlacement {
    key: PlacementKey,
    placement: Placement,

    /// How many parents lie between this placement and its root. Zero for
    /// a root. Sorting by depth writes parents before their children.
    depth: u32,

    /// Where the root is pinned, or null when the root is virtual.
    root: Option<RootPin>,
}

/// `PlannedPlacement.RootPin`
#[derive(Debug, Clone, Copy)]
struct RootPin {
    node: NodePtr,

    /// The page's position in the screen's page list, counting from
    /// zero at the oldest page.
    ordinal: u32,

    y: u16,
    x: u16,
}

/// Orders placement keys by image ID, then placement ID. This is the
/// final tiebreak in every placement order, so the encoding never depends
/// on hash map order.
fn key_less_than(lhs: PlacementKey, rhs: PlacementKey) -> bool {
    if lhs.image_id != rhs.image_id {
        return lhs.image_id < rhs.image_id;
    }
    let lhs_tag = lhs.placement_id.tag as u8;
    let rhs_tag = rhs.placement_id.tag as u8;
    if lhs_tag != rhs_tag {
        return lhs_tag < rhs_tag;
    }
    lhs.placement_id.id < rhs.placement_id.id
}

/// Which part of the snapshot a payload is written in
/// (Zig: `PlannedPayload.Stage`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
enum Stage {
    /// The image is placed in the viewport. Written right after READY,
    /// so what is on screen fills in first.
    Viewport,

    /// The image is placed on a history page. Written right after that
    /// page's KITTY_IMAGE_PAGE_PLACEMENTS, so images in scrollback
    /// arrive with the text around them and don't hold up the history
    /// pages before them.
    History,

    /// Everything else: images that are not placed, placed outside the
    /// viewport on an active page, or on a screen that is not shown.
    /// Written after all history, where nothing waits on them.
    Remaining,
}

/// One KITTY_IMAGE_PAYLOAD record to encode, and when to write it.
#[derive(Debug, Clone, Copy)]
struct PlannedPayload<'a> {
    image: &'a Image,

    /// The frame number, as the Kitty graphics protocol counts frames. 1 is
    /// the image itself and 2 and up are its animation frames.
    frame: u32,

    /// Which part of the snapshot the payload is written in.
    stage: Stage,

    /// For `.history`, the page the image is first placed on and that
    /// page's ordinal. The payload is written right after that page. Zero
    /// and null otherwise.
    node: Option<NodePtr>,
    ordinal: u32,

    /// True for the frame the image shows right now, which is the only
    /// one a renderer needs to draw it. See `Image.renderData`.
    displayed: bool,

    /// The image's position in `ScreenPlan.images`, oldest first.
    image_index: u32,
}

impl<'a> PlannedPayload<'a> {
    /// The pixels to encode.
    fn bytes(&self) -> &'a [u8] {
        match self.frame {
            1 => self.image.data.bytes().unwrap(),
            _ => &self.image.animation.as_ref().unwrap().frames[self.frame as usize - 2].data,
        }
    }

    /// The order payloads are written in: by stage, then newest history
    /// page first, then the displayed frame of every image before the other
    /// frames, then oldest image first, then by frame.
    fn less_than(lhs: &PlannedPayload<'_>, rhs: &PlannedPayload<'_>) -> bool {
        if lhs.stage != rhs.stage {
            return (lhs.stage as u8) < (rhs.stage as u8);
        }
        if lhs.ordinal != rhs.ordinal {
            return lhs.ordinal > rhs.ordinal;
        }
        if lhs.displayed != rhs.displayed {
            return lhs.displayed;
        }
        if lhs.image_index != rhs.image_index {
            return lhs.image_index < rhs.image_index;
        }
        lhs.frame < rhs.frame
    }
}

/// `ScreenPlan` in builds with Kitty graphics (always in the Rust port), so
/// callers can name the type unconditionally.
pub type Plan<'a> = ScreenPlan<'a>;

/// Where the history stage starts and ends in `ScreenPlan.payloads`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HistoryPayloads {
    start: usize,
    end: usize,
}

/// Everything needed to encode one screen's Kitty graphics records, worked
/// out once before encoding starts.
///
/// Encoding a screen's Kitty graphics takes more than one pass over its
/// storage. Images must be written oldest first, and placements must be
/// sorted so parents come before children and split by the page they are
/// pinned to. Pixel payloads are ordered so that what the reader can see
/// arrives first (see `PlannedPayload.Stage`). The plan does that work up
/// front, and then writes each record where it belongs:
///
/// - `encode_storage` right after the screen's SCREEN records.
/// - `encode_viewport_payloads` after READY.
/// - `encode_page_placements` right after each history page.
/// - `encode_remaining_payloads` after all history.
///
/// `init` returns null when the screen has no images and no in-progress
/// load. The snapshot then has no Kitty graphics records for that screen at
/// all, so a terminal without images encodes exactly as it did before these
/// records existed.
///
/// ```ignore
/// let Some(mut plan) = kitty::ScreenPlan::init(screen, Key::Primary) else { return };
/// plan.encode_storage(&mut stream)?;
/// // ... after READY ...
/// plan.encode_viewport_payloads(&mut stream)?;
/// // ... later, while encoding HISTORY ...
/// plan.encode_page_placements(history_page, &mut stream)?;
/// // ... after all history ...
/// plan.encode_remaining_payloads(&mut stream)?;
/// ```
pub struct ScreenPlan<'a> {
    screen: &'a TerminalScreen,
    key: TerminalScreenKey,

    /// Every image, oldest first.
    images: Vec<&'a Image>,

    /// Placements for KITTY_IMAGE_STORAGE: those rooted on a page of the SCREEN
    /// sequence, plus virtual ones. Parents come first.
    resident: Vec<PlannedPlacement>,

    /// Placements for KITTY_IMAGE_PAGE_PLACEMENTS: those rooted on a history
    /// page. Grouped by page, newest page first to match HISTORY, with
    /// parents first within each page.
    history: Vec<PlannedPlacement>,

    /// The next entry in `history` that `encode_page_placements` has not
    /// written yet.
    history_next: usize,

    /// The position of the SCREEN sequence's first page in the screen's
    /// page list. Pages before it are history. See `RootPin.ordinal`.
    first_resident: u32,

    /// KITTY_IMAGE_PAYLOAD records in the order they are written: the
    /// viewport stage, then the history stage grouped by page newest
    /// first, then the rest. See `PlannedPayload`.
    payloads: Vec<PlannedPayload<'a>>,

    /// Where the history stage starts and ends in `payloads`.
    history_payloads: HistoryPayloads,

    /// The next history-stage payload that `encode_page_placements` has not
    /// written yet.
    payload_next: usize,
}

/// Where each placed image goes: in the viewport, or on the newest
/// history page it is placed on.
#[derive(Debug, Clone, Copy)]
struct Where {
    stage: Stage,
    node: Option<NodePtr>,
    ordinal: u32,
}

impl Where {
    const VIEWPORT: Where = Where {
        stage: Stage::Viewport,
        node: None,
        ordinal: 0,
    };
    const REMAINING: Where = Where {
        stage: Stage::Remaining,
        node: None,
        ordinal: 0,
    };
}

impl<'a> ScreenPlan<'a> {
    /// Plan the Kitty graphics records for `screen`, which the snapshot
    /// names `key`. Returns null if there is nothing to encode. The plan
    /// points into the screen's storage, so the screen must not change
    /// while the plan is alive (enforced by the borrow).
    pub fn init(screen: &'a TerminalScreen, key: TerminalScreenKey) -> Option<ScreenPlan<'a>> {
        let storage = &screen.kitty_images;
        if storage.images.is_empty() && storage.loading.is_none() {
            return None;
        }

        // Images oldest first: the decoder restores them in this order and
        // stamps increasing generations, preserving eviction age.
        let mut images: Vec<&'a Image> = storage.images.values().collect();
        images.sort_unstable_by(order_by(|lhs: &&Image, rhs: &&Image| {
            if lhs.generation != rhs.generation {
                return lhs.generation < rhs.generation;
            }
            lhs.id < rhs.id
        }));

        // Page ordinals locate pinned roots: resident pages for
        // KITTY_IMAGE_STORAGE and history pages for
        // KITTY_IMAGE_PAGE_PLACEMENTS.
        let mut ordinals: HashMap<NodePtr, u32> = HashMap::new();
        let first_resident_node = screen.pages.get_top_left(point::Tag::Active).node;
        let mut first_resident: u32 = 0;
        {
            let mut node = screen.pages.pages.first;
            let mut ordinal: u32 = 0;
            while let Some(current) = node {
                if current == first_resident_node {
                    first_resident = ordinal;
                }
                ordinals.insert(current, ordinal);
                node = nd(current).next;
                ordinal += 1;
            }
        }

        let mut resident: Vec<PlannedPlacement> = Vec::new();
        let mut history: Vec<PlannedPlacement> = Vec::new();

        for (key, placement) in storage.placements.iter() {
            let Some(planned) = Self::plan(storage, &ordinals, *key, placement) else {
                continue;
            };
            if let Some(root) = planned.root {
                if root.ordinal < first_resident {
                    history.push(planned);
                    continue;
                }
            }
            resident.push(planned);
        }
        resident.sort_unstable_by(order_by(Self::resident_less_than));
        history.sort_unstable_by(order_by(Self::history_less_than));

        let payloads = Self::plan_payloads(screen, &images, &resident, &history);
        let history_start = payloads
            .iter()
            .position(|payload| payload.stage != Stage::Viewport)
            .unwrap_or(payloads.len());
        let history_end = payloads[history_start..]
            .iter()
            .position(|payload| payload.stage != Stage::History)
            .map_or(payloads.len(), |i| history_start + i);

        Some(ScreenPlan {
            screen,
            key,
            images,
            resident,
            history,
            history_next: 0,
            first_resident,
            payloads,
            history_payloads: HistoryPayloads {
                start: history_start,
                end: history_end,
            },
            payload_next: history_start,
        })
    }

    /// The order of `resident`. These placements share one record, so
    /// their pages don't matter: parents first, then by key.
    fn resident_less_than(lhs: &PlannedPlacement, rhs: &PlannedPlacement) -> bool {
        if lhs.depth != rhs.depth {
            return lhs.depth < rhs.depth;
        }
        key_less_than(lhs.key, rhs.key)
    }

    /// The order of `history`: grouped by page, newest page first to match
    /// the order HISTORY writes its pages, then parents first, then by key.
    fn history_less_than(lhs: &PlannedPlacement, rhs: &PlannedPlacement) -> bool {
        let lhs_ordinal = lhs.root.unwrap().ordinal;
        let rhs_ordinal = rhs.root.unwrap().ordinal;
        if lhs_ordinal != rhs_ordinal {
            return lhs_ordinal > rhs_ordinal;
        }
        if lhs.depth != rhs.depth {
            return lhs.depth < rhs.depth;
        }
        key_less_than(lhs.key, rhs.key)
    }

    /// Find one placement's root (see `PlannedPlacement`). Returns null
    /// when the placement can't be located and is left out of the
    /// snapshot: its root's pin was pruned, a parent is missing, or its
    /// parents form a loop.
    fn plan(
        storage: &ImageStorage,
        ordinals: &HashMap<NodePtr, u32>,
        key: PlacementKey,
        placement: &Placement,
    ) -> Option<PlannedPlacement> {
        let mut depth: u32 = 0;
        let mut current = placement;
        loop {
            match current.location {
                PlacementLocation::Relative(rel) => {
                    // A chain longer than the placement count must loop.
                    depth += 1;
                    if depth as usize > storage.placements.len() {
                        return None;
                    }
                    current = storage.placements.get(&rel.parent)?;
                }

                PlacementLocation::Virtual => {
                    return Some(PlannedPlacement {
                        key,
                        placement: *placement,
                        depth,
                        root: None,
                    });
                }

                PlacementLocation::Pin(pin) => {
                    // SAFETY: tracked pins of a placement stay valid while it
                    // is in storage.
                    let pin = unsafe { *pin.as_ptr() };
                    if pin.garbage {
                        return None;
                    }
                    return Some(PlannedPlacement {
                        key,
                        placement: *placement,
                        depth,
                        root: Some(RootPin {
                            node: pin.node,
                            ordinal: *ordinals.get(&pin.node)?,
                            y: pin.y,
                            x: pin.x,
                        }),
                    });
                }
            }
        }
    }

    /// Encode the KITTY_IMAGE_STORAGE record. Call this right after the
    /// screen's SCREEN and PAGE records, which is where decoders look for it.
    pub fn encode_storage(&self, destination: &mut record::Writer<'_>) -> Result<(), EncodeError> {
        let writer = destination.begin(record::Tag::KITTY_IMAGE_STORAGE);
        if let Err(e) = self.encode_storage_payload(writer) {
            destination.cancel();
            return Err(e);
        }
        destination.finish()
    }

    fn encode_storage_payload(&self, writer: &mut dyn io::Writer) -> io::Result<()> {
        let storage = &self.screen.kitty_images;
        encode_key(writer, self.key)?;
        io::write_int::<u32>(writer, storage.next_image_id)?;
        io::write_int::<u32>(writer, storage.next_internal_placement_id)?;
        io::write_int::<u32>(writer, self.images.len() as u32)?;
        io::write_int::<u32>(writer, self.resident.len() as u32)?;
        writer.write_byte(storage.loading.is_some() as u8)?;

        for img in &self.images {
            encode_image(writer, img)?;
        }
        for planned in &self.resident {
            encode_placement(writer, planned, PinContext::Storage(self.first_resident))?;
        }
        if let Some(loading) = &storage.loading {
            encode_loading(writer, loading)?;
        }
        Ok(())
    }

    /// Plan a payload for every image and frame whose pixels are present,
    /// sorted into the order they are written. See `PlannedPayload`.
    fn plan_payloads(
        screen: &TerminalScreen,
        images: &[&'a Image],
        resident: &[PlannedPlacement],
        history: &[PlannedPlacement],
    ) -> Vec<PlannedPayload<'a>> {
        // Where each placed image goes: in the viewport, or on the newest
        // history page it is placed on.
        let mut placed: HashMap<u32, Where> = HashMap::new();

        // The viewport may be scrolled into history, so every placement is
        // checked against it. An image placed more than once takes the
        // earliest stage of any of its placements, and among history
        // placements the newest page, which comes first.
        for (items, group_stage) in [(resident, Stage::Remaining), (history, Stage::History)] {
            for planned in items {
                let candidate: Where = 'candidate: {
                    // A virtual placement is drawn wherever its placeholder
                    // characters are, which is usually on screen.
                    let Some(root) = planned.root else {
                        break 'candidate Where::VIEWPORT;
                    };
                    let pin = Pin {
                        node: root.node,
                        y: root.y,
                        x: root.x,
                        garbage: false,
                    };
                    if screen.pages.point_from_pin(point::Tag::Viewport, pin).is_some() {
                        break 'candidate Where::VIEWPORT;
                    }
                    match group_stage {
                        Stage::History => Where {
                            stage: Stage::History,
                            node: Some(root.node),
                            ordinal: root.ordinal,
                        },
                        _ => Where::REMAINING,
                    }
                };
                match placed.get_mut(&planned.key.image_id) {
                    None => {
                        placed.insert(planned.key.image_id, candidate);
                    }
                    Some(existing) => {
                        if (candidate.stage as u8) < (existing.stage as u8) {
                            *existing = candidate;
                        }
                    }
                }
            }
        }

        let mut result: Vec<PlannedPayload<'a>> = Vec::new();
        for (index, &img) in images.iter().enumerate() {
            let where_ = placed.get(&img.id).copied().unwrap_or(Where::REMAINING);

            // The displayed frame is the current animation frame, or the
            // image itself.
            let displayed: u32 = match &img.animation {
                Some(anim) => anim.current_index + 1,
                None => 1,
            };

            let frame_count: u32 = match &img.animation {
                Some(anim) => (anim.frames.len() + 1) as u32,
                None => 1,
            };
            for frame in 1..=frame_count {
                let present = match frame {
                    1 => !img.data.is_pending(),
                    _ => !img.animation.as_ref().unwrap().frames[frame as usize - 2].is_pending(),
                };
                if !present {
                    continue;
                }
                result.push(PlannedPayload {
                    image: img,
                    frame,
                    stage: where_.stage,
                    node: where_.node,
                    ordinal: where_.ordinal,
                    displayed: frame == displayed,
                    image_index: index as u32,
                });
            }
        }
        result.sort_unstable_by(order_by(PlannedPayload::less_than));
        result
    }

    /// Encode a KITTY_IMAGE_PAYLOAD record for each image placed in the
    /// viewport, or nothing when there are none. Call this after READY.
    /// For the screen that is shown, call it first thing after READY so the
    /// reader can draw what is on screen as soon as possible.
    pub fn encode_viewport_payloads(&self, destination: &mut record::Writer<'_>) -> Result<(), EncodeError> {
        for payload in &self.payloads[..self.history_payloads.start] {
            encode_payload(destination, self.key, payload)?;
        }
        Ok(())
    }

    /// Encode a KITTY_IMAGE_PAYLOAD record for each image and frame whose
    /// pixels have not been written by `encode_viewport_payloads` or
    /// `encode_page_placements`. Call this after the screen's HISTORY, which
    /// must have been encoded with this plan.
    pub fn encode_remaining_payloads(&self, destination: &mut record::Writer<'_>) -> Result<(), EncodeError> {
        debug_assert!(self.payload_next == self.history_payloads.end);
        for payload in &self.payloads[self.history_payloads.end..] {
            encode_payload(destination, self.key, payload)?;
        }
        Ok(())
    }

    /// Encode the KITTY_IMAGE_PAGE_PLACEMENTS record for one history page, if
    /// any placement is pinned to it, and nothing otherwise. Then encode the
    /// KITTY_IMAGE_PAYLOAD records of the images that are first placed on
    /// this page, so they arrive with the text around them. Call this right
    /// after the page's PAGE record. Pages must be visited newest first, in
    /// the same order HISTORY encodes them.
    pub fn encode_page_placements(
        &mut self,
        node: NodePtr,
        destination: &mut record::Writer<'_>,
    ) -> Result<(), EncodeError> {
        let start = self.history_next;
        let mut end = start;
        while end < self.history.len() && self.history[end].root.unwrap().node == node {
            end += 1;
        }
        if end > start {
            self.history_next = end;

            let writer = destination.begin(record::Tag::KITTY_IMAGE_PAGE_PLACEMENTS);
            let result = (|| -> io::Result<()> {
                encode_key(writer, self.key)?;
                io::write_int::<u32>(writer, (end - start) as u32)?;
                for planned in &self.history[start..end] {
                    encode_placement(writer, planned, PinContext::Page)?;
                }
                Ok(())
            })();
            if let Err(e) = result {
                destination.cancel();
                return Err(e);
            }
            destination.finish()?;
        }

        while self.payload_next < self.history_payloads.end && self.payloads[self.payload_next].node == Some(node) {
            encode_payload(destination, self.key, &self.payloads[self.payload_next])?;
            self.payload_next += 1;
        }
        Ok(())
    }
}

fn encode_image(writer: &mut dyn io::Writer, img: &Image) -> io::Result<()> {
    io::write_int::<u32>(writer, img.id)?;
    io::write_int::<u32>(writer, img.number)?;
    io::write_int::<u32>(writer, img.width)?;
    io::write_int::<u32>(writer, img.height)?;
    writer.write_byte(encode_format(img.format))?;
    writer.write_byte(image_flags(img).to_bits())?;
    io::write_int::<u64>(writer, img.data.len() as u64)?;

    let Some(anim) = &img.animation else {
        return writer.write_byte(0);
    };
    writer.write_byte(1)?;
    writer.write_byte(match anim.state {
        animation::State::Stopped => 0,
        animation::State::Loading => 1,
        animation::State::Running => 2,
    })?;
    io::write_int::<u32>(writer, anim.max_loops)?;
    io::write_int::<u32>(writer, anim.current_loop)?;
    io::write_int::<u32>(writer, anim.current_index)?;
    io::write_int::<u32>(writer, anim.root_gap_ms)?;
    io::write_int::<u32>(writer, anim.frames.len() as u32)?;
    for frame in &anim.frames {
        io::write_int::<u32>(writer, frame.gap_ms)?;
        io::write_int::<u64>(writer, frame.storage_size() as u64)?;
    }
    Ok(())
}

/// Which record a placement is written to, which decides how its pin is
/// written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PinContext {
    /// KITTY_IMAGE_STORAGE. The pin names its page by position in the SCREEN
    /// sequence. The value is `ScreenPlan.first_resident`, which is
    /// subtracted from the page's position in the full page list.
    Storage(u32),

    /// KITTY_IMAGE_PAGE_PLACEMENTS. The pin is always on the history page right
    /// before the record, so only the row and column are written.
    Page,
}

/// `std.meta.Tag(PinContext)`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PinContextTag {
    Storage,
    Page,
}

fn encode_placement_key(writer: &mut dyn io::Writer, key: PlacementKey) -> io::Result<()> {
    io::write_int::<u32>(writer, key.image_id)?;
    writer.write_byte(key.placement_id.tag as u8)?;
    io::write_int::<u32>(writer, key.placement_id.id)
}

fn encode_placement(writer: &mut dyn io::Writer, planned: &PlannedPlacement, context: PinContext) -> io::Result<()> {
    let p = &planned.placement;
    encode_placement_key(writer, planned.key)?;
    match p.location {
        PlacementLocation::Pin(pin) => {
            // SAFETY: the placement's tracked pin is live.
            let pin = unsafe { *pin.as_ptr() };
            writer.write_byte(0)?;
            match context {
                PinContext::Storage(first) => {
                    io::write_int::<u16>(writer, (planned.root.unwrap().ordinal - first) as u16)?;
                }
                PinContext::Page => {}
            }
            io::write_int::<u16>(writer, pin.y)?;
            io::write_int::<u16>(writer, pin.x)?;
        }
        PlacementLocation::Virtual => writer.write_byte(1)?,
        PlacementLocation::Relative(rel) => {
            writer.write_byte(2)?;
            encode_placement_key(writer, rel.parent)?;
            write_i32(writer, rel.horizontal_offset)?;
            write_i32(writer, rel.vertical_offset)?;
        }
    }
    io::write_int::<u32>(writer, p.x_offset)?;
    io::write_int::<u32>(writer, p.y_offset)?;
    io::write_int::<u32>(writer, p.source_x)?;
    io::write_int::<u32>(writer, p.source_y)?;
    io::write_int::<u32>(writer, p.source_width)?;
    io::write_int::<u32>(writer, p.source_height)?;
    io::write_int::<u32>(writer, p.columns)?;
    io::write_int::<u32>(writer, p.rows)?;
    write_i32(writer, p.z)
}

fn encode_loading(writer: &mut dyn io::Writer, loading: &LoadingImage) -> io::Result<()> {
    let img = &loading.image;
    io::write_int::<u32>(writer, img.id)?;
    io::write_int::<u32>(writer, img.number)?;
    io::write_int::<u32>(writer, img.width)?;
    io::write_int::<u32>(writer, img.height)?;
    writer.write_byte(encode_format(img.format))?;
    writer.write_byte(encode_compression(img.compression))?;
    writer.write_byte(image_flags(img).to_bits())?;
    writer.write_byte(match loading.quiet {
        graphics_command::Quiet::No => 0,
        graphics_command::Quiet::Ok => 1,
        graphics_command::Quiet::Failures => 2,
    })?;
    io::write_int::<u32>(writer, loading.response.id)?;
    io::write_int::<u32>(writer, loading.response.image_number)?;
    io::write_int::<u32>(writer, loading.response.placement_id)?;
    io::write_int::<u32>(writer, loading.response.frame)?;

    if let Some(d) = &loading.display {
        writer.write_byte(1)?;
        for value in [
            d.image_id,
            d.image_number,
            d.placement_id,
            d.x,
            d.y,
            d.width,
            d.height,
            d.x_offset,
            d.y_offset,
            d.columns,
            d.rows,
        ] {
            io::write_int::<u32>(writer, value)?;
        }
        writer.write_byte(match d.cursor_movement {
            graphics_command::CursorMovement::After => 0,
            graphics_command::CursorMovement::None => 1,
        })?;
        writer.write_byte(d.virtual_placement as u8)?;
        io::write_int::<u32>(writer, d.parent_id)?;
        io::write_int::<u32>(writer, d.parent_placement_id)?;
        write_i32(writer, d.horizontal_offset)?;
        write_i32(writer, d.vertical_offset)?;
        write_i32(writer, d.z)?;
    } else {
        writer.write_byte(0)?;
    }

    if let Some(frame) = &loading.frame {
        let f = &frame.cmd;
        let t = &f.transmission;
        writer.write_byte(1)?;
        writer.write_byte(encode_format(t.format))?;
        writer.write_byte(match t.medium {
            graphics_command::Medium::Direct => 0,
            graphics_command::Medium::File => 1,
            graphics_command::Medium::TemporaryFile => 2,
            graphics_command::Medium::SharedMemory => 3,
        })?;
        for value in [
            t.width,
            t.height,
            t.size,
            t.offset,
            t.image_id,
            t.image_number,
            t.placement_id,
        ] {
            io::write_int::<u32>(writer, value)?;
        }
        writer.write_byte(encode_compression(t.compression))?;
        writer.write_byte(t.more_chunks as u8)?;
        io::write_int::<u32>(writer, t.usage.to_bits())?;
        io::write_int::<u32>(writer, f.x)?;
        io::write_int::<u32>(writer, f.y)?;
        io::write_int::<u32>(writer, f.create_frame)?;
        io::write_int::<u32>(writer, f.edit_frame)?;
        write_i32(writer, f.gap_ms)?;
        writer.write_byte(match f.composition_mode {
            graphics_command::CompositionMode::AlphaBlend => 0,
            graphics_command::CompositionMode::Overwrite => 1,
        })?;
        io::write_int::<u32>(writer, f.background.to_bits())?;
    } else {
        writer.write_byte(0)?;
    }

    io::write_int::<u32>(writer, loading.data.len() as u32)?;
    writer.write_all(&loading.data)
}

type Compression = graphics_command::Compression;

fn encode_compression(value: Compression) -> u8 {
    match value {
        Compression::None => 0,
        Compression::ZlibDeflate => 1,
    }
}

fn decode_compression(raw: u8) -> Option<Compression> {
    match raw {
        0 => Some(Compression::None),
        1 => Some(Compression::ZlibDeflate),
        _ => None,
    }
}

/// The number of KITTY_IMAGE_PAYLOAD bytes before the pixel bytes.
const PAYLOAD_HEADER_LEN: usize = 18;

/// Encode one KITTY_IMAGE_PAYLOAD record.
fn encode_payload(
    destination: &mut record::Writer<'_>,
    key: TerminalScreenKey,
    payload: &PlannedPayload<'_>,
) -> Result<(), EncodeError> {
    let bytes = payload.bytes();
    let writer = destination.begin(record::Tag::KITTY_IMAGE_PAYLOAD);
    let result = (|| -> io::Result<()> {
        encode_key(writer, key)?;
        io::write_int::<u32>(writer, payload.image.id)?;
        io::write_int::<u32>(writer, payload.frame)?;
        io::write_int::<u64>(writer, bytes.len() as u64)?;
        debug_assert!(writer.len() == PAYLOAD_HEADER_LEN);
        io::Writer::write_all(writer, bytes)
    })();
    if let Err(e) = result {
        destination.cancel();
        return Err(e);
    }
    destination.finish()
}

// ---------------------------------------------------------------------------
// Decoding

/// The Kitty graphics a screen had when the snapshot reached READY. See
/// `snapshot::Decoded.kitty` for how to show progress with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    /// Images restored into the screen's storage.
    pub images: u32,

    /// Bytes of image and frame pixels that have not arrived yet.
    pub pending_bytes: u64,
}

impl Counts {
    /// Count the images in `storage` and the bytes of their pixels that
    /// are still pending.
    pub fn init(storage: &ImageStorage) -> Counts {
        let mut result = Counts::default();
        for img in storage.images.values() {
            result.images += 1;
            if img.data.is_pending() {
                result.pending_bytes += img.data.len() as u64;
            }
            let Some(anim) = &img.animation else {
                continue;
            };
            for frame in &anim.frames {
                if frame.is_pending() {
                    result.pending_bytes += frame.pending_len as u64;
                }
            }
        }
        result
    }
}

/// Decode one KITTY_IMAGE_STORAGE record into `screen`.
///
/// `record_header` is the record's header, already read from `source`. A
/// header for any other kind of record returns `UnexpectedRecordTag`.
/// See "Decoding Records" in `record.rs`.
///
/// A KITTY_IMAGE_STORAGE record comes right after the SCREEN records of the
/// screen it belongs to. Pass that screen's `key` and the `screen` freshly
/// restored from those records, with no images yet. A record that names a
/// different screen returns `UnexpectedKittyKey`.
///
/// Images are restored with their pixels pending, oldest first, under the
/// screen's current storage limit. An image that does not fit evicts older
/// images, exactly as a live transmission would, so the newest images are
/// kept. Invalid images and placements, and placements whose image or
/// parent was dropped, are left out rather than failing the snapshot.
///
/// ```ignore
/// // Right after restoring the primary screen from its SCREEN records:
/// let header = record::next_header(source)?;
/// if header.tag == record::Tag::KITTY_IMAGE_STORAGE {
///     kitty::decode_storage(source, header, Key::Primary, primary)?;
/// }
/// ```
pub fn decode_storage(
    source: &mut dyn io::Reader,
    record_header: record::Header,
    key: TerminalScreenKey,
    screen: &mut TerminalScreen,
) -> Result<(), DecodeError> {
    let mut record_reader = record::Reader::init_header(source, record_header);
    if record_header.tag != record::Tag::KITTY_IMAGE_STORAGE {
        return Err(Error::UnexpectedRecordTag);
    }

    let (next_image_id, next_internal_placement_id) = {
        let reader = record_reader.payload_reader();
        if decode_key(reader)? != key {
            return Err(Error::UnexpectedKittyKey);
        }

        let next_image_id = io::read_int::<u32>(reader)?;
        let next_internal_placement_id = io::read_int::<u32>(reader)?;
        let image_count = io::read_int::<u32>(reader)?;
        let placement_count = io::read_int::<u32>(reader)?;
        let loading_present = reader.take_byte()?;

        for _ in 0..image_count {
            let mut decoded = DecodedImage::decode(reader)?;
            decoded.restore(screen)?;
        }

        // Pin locations index the restored SCREEN pages.
        let mut nodes: Vec<NodePtr> = Vec::new();
        {
            let mut node = screen.pages.pages.first;
            while let Some(current) = node {
                nodes.push(current);
                node = nd(current).next;
            }
        }
        for _ in 0..placement_count {
            let decoded = DecodedPlacement::decode(reader, PinContextTag::Storage)?;
            decoded.restore(screen, PinTarget::Storage(&nodes), None)?;
        }

        if loading_present != 0 {
            decode_loading(reader, record_header.payload_len, &mut screen.kitty_images)?;
        }
        (next_image_id, next_internal_placement_id)
    };
    record_reader.finish()?;

    let storage = &mut screen.kitty_images;
    storage.next_image_id = next_image_id;
    storage.next_internal_placement_id = next_internal_placement_id;
    Ok(())
}

/// One decoded image entry.
struct DecodedImage {
    image: Image,

    /// The decoded animation with every frame pending, owned until
    /// `restore` moves it into storage.
    animation: Option<Box<Animation>>,

    /// False when the entry cannot describe a stored image.
    valid: bool,
}

impl DecodedImage {
    fn decode(reader: &mut dyn io::Reader) -> Result<DecodedImage, DecodeError> {
        let id = io::read_int::<u32>(reader)?;
        let number = io::read_int::<u32>(reader)?;
        let width = io::read_int::<u32>(reader)?;
        let height = io::read_int::<u32>(reader)?;
        let format = decode_format(reader.take_byte()?);
        let flags = ImageFlags::from_bits(reader.take_byte()?);
        let data_len = io::read_int::<u64>(reader)?;

        let mut valid = 'valid: {
            if id == 0 {
                break 'valid false;
            }
            let Some(f) = format else {
                break 'valid false;
            };
            let Some(expected) = image_len(width, height, f) else {
                break 'valid false;
            };
            expected as u64 == data_len
        };

        let mut result = DecodedImage {
            image: Image {
                id,
                number,
                width,
                height,
                format: format.unwrap_or(Format::Rgba),
                data: Data::Pending(if valid { data_len as usize } else { 0 }),
                metadata: Metadata {
                    transient: flags.transient,
                    implicit_id: flags.implicit_id,
                    ..Metadata::default()
                },
                ..Image::default()
            },
            animation: None,
            valid,
        };

        if reader.take_byte()? == 0 {
            return Ok(result);
        }
        let state = match reader.take_byte()? {
            1 => animation::State::Loading,
            2 => animation::State::Running,
            _ => animation::State::Stopped,
        };
        let max_loops = io::read_int::<u32>(reader)?;
        let current_loop = io::read_int::<u32>(reader)?;
        let current_index = io::read_int::<u32>(reader)?;
        let root_gap_ms = io::read_int::<u32>(reader)?;
        let frame_count = io::read_int::<u32>(reader)?;

        let mut anim = Box::new(Animation {
            state,
            max_loops,
            current_loop,
            root_gap_ms,
            ..Animation::default()
        });

        // Frames are always full-size RGBA. Every frame entry is read even
        // for an invalid image so the payload stays aligned.
        let frame_len: Option<usize> = (width as usize * height as usize).checked_mul(4);
        for _ in 0..frame_count {
            let gap_ms = io::read_int::<u32>(reader)?;
            let len = io::read_int::<u64>(reader)?;
            if !valid {
                continue;
            }
            match frame_len {
                Some(frame_len) if frame_len != 0 && len == frame_len as u64 => {
                    anim.frames.push(animation::Frame {
                        data: Vec::new(),
                        gap_ms,
                        pending_len: frame_len,
                    });
                }
                _ => {
                    valid = false;
                    continue;
                }
            }
        }
        anim.current_index = if current_index as usize <= anim.frames.len() {
            current_index
        } else {
            0
        };
        result.animation = Some(anim);
        result.valid = valid;
        Ok(result)
    }

    /// Add the image to storage as a pending image, unless it is invalid
    /// or larger than the whole storage limit.
    fn restore(&mut self, screen: &mut TerminalScreen) -> Result<(), DecodeError> {
        if !self.valid {
            return Ok(());
        }
        let TerminalScreen {
            pages,
            kitty_images: storage,
            ..
        } = screen;

        // An image that cannot fit even in an empty storage is dropped
        // rather than evicting everything else for nothing.
        let base_len = self.image.data.len();
        let frame_bytes = self.animation.as_ref().map_or(0, |a| a.frame_bytes());
        if base_len + frame_bytes > storage.total_limit {
            return Ok(());
        }

        let id = self.image.id;
        storage.add_pending_image(pages, std::mem::take(&mut self.image))?;
        let Some(anim) = self.animation.take() else {
            return Ok(());
        };
        if frame_bytes > 0 {
            // Older images could not make room. Keep the image itself and
            // drop its animation.
            if storage.reserve_animation_bytes(pages, id, frame_bytes).is_err() {
                return Ok(());
            }
        }
        storage.images.get_mut(&id).unwrap().animation = Some(anim);
        Ok(())
    }
}

/// Where pinned placements are located while decoding.
#[derive(Debug, Clone, Copy)]
enum PinTarget<'a> {
    /// KITTY_IMAGE_STORAGE: the restored screen's pages in SCREEN order.
    Storage(&'a [NodePtr]),

    /// KITTY_IMAGE_PAGE_PLACEMENTS: the history page that was just prepended.
    Page(NodePtr),
}

/// `DecodedPlacement.Location`
#[derive(Debug, Clone, Copy)]
enum DecodedLocation {
    Pin { page: u16, y: u16, x: u16 },
    Virtual,
    Relative(Relative),

    /// Not a location the placement's record may carry.
    Invalid,
}

/// One decoded placement entry.
struct DecodedPlacement {
    key: Option<PlacementKey>,
    location: DecodedLocation,
    placement: Placement,
}

impl DecodedPlacement {
    fn decode_placement_key(reader: &mut dyn io::Reader) -> Result<Option<PlacementKey>, DecodeError> {
        let image_id = io::read_int::<u32>(reader)?;
        let tag_raw = reader.take_byte()?;
        let id = io::read_int::<u32>(reader)?;
        let tag = match tag_raw {
            0 => PlacementTag::Internal,
            1 => PlacementTag::External,
            _ => return Ok(None),
        };
        // An explicit placement ID of zero means "assign one internally".
        if tag == PlacementTag::External && id == 0 {
            return Ok(None);
        }
        Ok(Some(PlacementKey {
            image_id,
            placement_id: PlacementId { tag, id },
        }))
    }

    fn decode(reader: &mut dyn io::Reader, context: PinContextTag) -> Result<DecodedPlacement, DecodeError> {
        let key = Self::decode_placement_key(reader)?;
        let location = match reader.take_byte()? {
            0 => {
                let page = match context {
                    PinContextTag::Storage => io::read_int::<u16>(reader)?,
                    PinContextTag::Page => 0,
                };
                let y = io::read_int::<u16>(reader)?;
                let x = io::read_int::<u16>(reader)?;
                DecodedLocation::Pin { page, y, x }
            }
            1 => match context {
                PinContextTag::Storage => DecodedLocation::Virtual,
                PinContextTag::Page => DecodedLocation::Invalid,
            },
            2 => {
                let parent = Self::decode_placement_key(reader)?;
                let horizontal = read_i32(reader)?;
                let vertical = read_i32(reader)?;
                match parent {
                    Some(p) => DecodedLocation::Relative(Relative {
                        parent: p,
                        horizontal_offset: horizontal,
                        vertical_offset: vertical,
                    }),
                    None => DecodedLocation::Invalid,
                }
            }
            _ => DecodedLocation::Invalid,
        };

        let x_offset = io::read_int::<u32>(reader)?;
        let y_offset = io::read_int::<u32>(reader)?;
        let source_x = io::read_int::<u32>(reader)?;
        let source_y = io::read_int::<u32>(reader)?;
        let source_width = io::read_int::<u32>(reader)?;
        let source_height = io::read_int::<u32>(reader)?;
        let columns = io::read_int::<u32>(reader)?;
        let rows = io::read_int::<u32>(reader)?;
        let z = read_i32(reader)?;
        Ok(DecodedPlacement {
            key,
            location,
            placement: Placement {
                location: PlacementLocation::Virtual,
                x_offset,
                y_offset,
                source_x,
                source_y,
                source_width,
                source_height,
                columns,
                rows,
                z,
            },
        })
    }

    /// Insert the placement with its exact key. It is left out if its key
    /// or location is invalid, its image or parent is missing, or the key
    /// is already in use. With a `ready_generation` (see `PageTarget`), the
    /// image must also be one the snapshot restored, not one that live
    /// input sent since.
    fn restore(
        &self,
        screen: &mut TerminalScreen,
        pins: PinTarget<'_>,
        ready_generation: Option<u64>,
    ) -> Result<(), DecodeError> {
        let TerminalScreen {
            pages,
            kitty_images: storage,
            ..
        } = screen;
        let Some(key) = self.key else {
            return Ok(());
        };
        let Some(img) = storage.images.get(&key.image_id) else {
            return Ok(());
        };
        if let Some(ready) = ready_generation {
            if img.transmission > ready {
                return Ok(());
            }
        }
        if storage.placements.contains_key(&key) {
            return Ok(());
        }
        let placement_count = img.metadata.placement_count;

        let mut placement = self.placement;
        match self.location {
            DecodedLocation::Invalid => return Ok(()),
            DecodedLocation::Virtual => placement.location = PlacementLocation::Virtual,
            DecodedLocation::Relative(rel) => {
                // Parents always precede their children.
                if !storage.placements.contains_key(&rel.parent) {
                    return Ok(());
                }
                placement.location = PlacementLocation::Relative(rel);
            }
            DecodedLocation::Pin { page, y, x } => {
                let node = match pins {
                    PinTarget::Storage(nodes) => {
                        if (page as usize) < nodes.len() {
                            nodes[page as usize]
                        } else {
                            return Ok(());
                        }
                    }
                    PinTarget::Page(node) => node,
                };
                if y >= nd(node).rows() || x >= nd(node).cols() {
                    return Ok(());
                }
                placement.location = PlacementLocation::Pin(pages.track_pin(Pin {
                    node,
                    y,
                    x,
                    garbage: false,
                })?);
            }
        }

        // Zig: `std.math.add(u30, ...)`.
        let count = match placement_count.checked_add(1) {
            Some(count) if count <= Metadata::MAX_PLACEMENT_COUNT => count,
            _ => {
                placement.deinit(pages);
                return Ok(());
            }
        };
        storage.placements.insert(key, placement);
        storage.images.get_mut(&key.image_id).unwrap().metadata.placement_count = count;
        storage.mark_mutated();
        Ok(())
    }
}

/// Decode the in-progress load and install it, unless the storage cannot
/// accept transmissions or the load is invalid.
fn decode_loading(reader: &mut dyn io::Reader, payload_len: u32, storage: &mut ImageStorage) -> Result<(), DecodeError> {
    let id = io::read_int::<u32>(reader)?;
    let number = io::read_int::<u32>(reader)?;
    let width = io::read_int::<u32>(reader)?;
    let height = io::read_int::<u32>(reader)?;
    let format = decode_format(reader.take_byte()?);
    let compression = decode_compression(reader.take_byte()?);
    let flags = ImageFlags::from_bits(reader.take_byte()?);
    let quiet = match reader.take_byte()? {
        0 => Some(graphics_command::Quiet::No),
        1 => Some(graphics_command::Quiet::Ok),
        2 => Some(graphics_command::Quiet::Failures),
        _ => None,
    };
    let response = graphics_command::Response {
        id: io::read_int::<u32>(reader)?,
        image_number: io::read_int::<u32>(reader)?,
        placement_id: io::read_int::<u32>(reader)?,
        frame: io::read_int::<u32>(reader)?,
        ..graphics_command::Response::default()
    };

    let mut valid = format.is_some() && compression.is_some() && quiet.is_some();

    let display: Option<graphics_command::Display> = match reader.take_byte()? {
        0 => None,
        _ => {
            let mut d = graphics_command::Display::default();
            for field in [
                &mut d.image_id,
                &mut d.image_number,
                &mut d.placement_id,
                &mut d.x,
                &mut d.y,
                &mut d.width,
                &mut d.height,
                &mut d.x_offset,
                &mut d.y_offset,
                &mut d.columns,
                &mut d.rows,
            ] {
                *field = io::read_int::<u32>(reader)?;
            }
            d.cursor_movement = match reader.take_byte()? {
                0 => graphics_command::CursorMovement::After,
                1 => graphics_command::CursorMovement::None,
                _ => {
                    valid = false;
                    graphics_command::CursorMovement::After
                }
            };
            d.virtual_placement = reader.take_byte()? != 0;
            d.parent_id = io::read_int::<u32>(reader)?;
            d.parent_placement_id = io::read_int::<u32>(reader)?;
            d.horizontal_offset = read_i32(reader)?;
            d.vertical_offset = read_i32(reader)?;
            d.z = read_i32(reader)?;
            Some(d)
        }
    };

    let frame: Option<graphics_command::AnimationFrameLoading> = match reader.take_byte()? {
        0 => None,
        _ => {
            let mut f = graphics_command::AnimationFrameLoading::default();
            let t = &mut f.transmission;
            t.format = decode_format(reader.take_byte()?).unwrap_or_else(|| {
                valid = false;
                Format::Rgba
            });
            t.medium = match reader.take_byte()? {
                0 => graphics_command::Medium::Direct,
                1 => graphics_command::Medium::File,
                2 => graphics_command::Medium::TemporaryFile,
                3 => graphics_command::Medium::SharedMemory,
                _ => {
                    valid = false;
                    graphics_command::Medium::Direct
                }
            };
            for field in [
                &mut t.width,
                &mut t.height,
                &mut t.size,
                &mut t.offset,
                &mut t.image_id,
                &mut t.image_number,
                &mut t.placement_id,
            ] {
                *field = io::read_int::<u32>(reader)?;
            }
            t.compression = decode_compression(reader.take_byte()?).unwrap_or_else(|| {
                valid = false;
                Compression::None
            });
            t.more_chunks = reader.take_byte()? != 0;
            t.usage = graphics_command::Usage::from_bits(io::read_int::<u32>(reader)?);
            f.x = io::read_int::<u32>(reader)?;
            f.y = io::read_int::<u32>(reader)?;
            f.create_frame = io::read_int::<u32>(reader)?;
            f.edit_frame = io::read_int::<u32>(reader)?;
            f.gap_ms = read_i32(reader)?;
            f.composition_mode = match reader.take_byte()? {
                0 => graphics_command::CompositionMode::AlphaBlend,
                1 => graphics_command::CompositionMode::Overwrite,
                _ => {
                    valid = false;
                    graphics_command::CompositionMode::AlphaBlend
                }
            };
            f.background = graphics_command::Background::from_bits(io::read_int::<u32>(reader)?);
            Some(f)
        }
    };

    // The received bytes are bounded by the record itself, so a corrupt
    // length cannot force a large allocation.
    let data_len = io::read_int::<u32>(reader)?;
    if data_len > payload_len {
        return Err(Error::EndOfStream);
    }
    let mut data = vec![0u8; data_len as usize];
    reader.read_slice_all(&mut data)?;

    if !valid || !storage.enabled() {
        return Ok(());
    }

    let mut loading = LoadingImage {
        image: Image {
            id,
            number,
            width,
            height,
            format: format.unwrap(),
            compression: compression.unwrap(),
            metadata: Metadata {
                transient: flags.transient,
                implicit_id: flags.implicit_id,
                ..Metadata::default()
            },
            ..Image::default()
        },
        data: Vec::new(),
        display,
        frame: None,
        quiet: quiet.unwrap(),
        response,
        temporary_directory: match &storage.image_limits.temporary_file {
            TemporaryFile::Enabled { directory } => Some(directory.clone()),
            TemporaryFile::Disabled => None,
        },
    };

    // A frame load completes only against the image it started on, which
    // is identified by its generation. Stamps are process-local, so use
    // the restored image's fresh one.
    if let Some(f) = frame {
        loading.frame = Some(FrameContext {
            cmd: f,
            image_generation: storage
                .image_ptr_by_id_or_number(id, number)
                .map_or(0, |img| img.generation),
        });
    }

    // add_data enforces the same size bound as live loading.
    match loading.add_data(&data) {
        Ok(()) => {}
        Err(crate::terminal::kitty::graphics_image::Error::OutOfMemory) => return Err(Error::OutOfMemory),
        Err(_) => {
            log::debug!(target: LOG_TARGET, "dropping invalid in-progress load");
            return Ok(());
        }
    }

    storage.loading = Some(Box::new(loading));
    Ok(())
}

/// A screen that KITTY_IMAGE_PAYLOAD records may complete images on. See
/// `decode_payload`.
pub struct PayloadTarget<'a> {
    /// The screen restored at READY.
    pub screen: &'a mut TerminalScreen,

    /// The screen's image storage `generation` at READY. An image whose
    /// `transmission` is newer than this was sent by live input after
    /// READY, and only shares an ID with the snapshot's image. See
    /// `PageTarget.ready_generation`.
    pub ready_generation: u64,
}

/// The screens that KITTY_IMAGE_PAYLOAD records may complete images on, by
/// screen key (Zig: `std.EnumMap`). See `decode_payload`.
pub type PayloadTargets<'a> = [Option<PayloadTarget<'a>>; TerminalScreenKey::COUNT];

/// What one KITTY_IMAGE_PAYLOAD record held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Payload {
    /// The screen the record names.
    pub key: TerminalScreenKey,

    /// The image the pixels are for.
    pub image_id: u32,

    /// The frame number, as the Kitty graphics protocol counts frames. 1 is
    /// the image itself and 2 and up are its animation frames.
    pub frame: u32,

    /// The number of pixel bytes attached to the image, or zero when the
    /// payload was dropped.
    pub bytes: usize,
}

/// Decode one KITTY_IMAGE_PAYLOAD record and attach its pixels to the pending
/// image or animation frame they belong to.
///
/// `record_header` is the record's header, already read from `source`. A
/// header for any other kind of record returns `UnexpectedRecordTag`.
/// See "Decoding Records" in `record.rs`.
///
/// The record names its own screen, and `targets` gives the screen to use
/// for each key. Leave out a screen that is gone, because live input
/// removed it or replaced it with a new one since READY. The result
/// reports the record's screen in `key`, so the caller can check that the
/// snapshot declared it.
///
/// The pixels are attached only if `targets` has the record's screen, that
/// screen still has the image the snapshot restored, and the image or
/// frame is still pending with the same length. Otherwise the record is
/// read and checked but its pixels are dropped, and the result reports
/// zero bytes.
///
/// ```ignore
/// let mut targets: kitty::PayloadTargets = [None, None];
/// targets[Key::Primary as usize] = Some(kitty::PayloadTarget {
///     screen: primary,
///     ready_generation: primary_generation_at_ready,
/// });
///
/// let header = record::next_header(source)?;
/// if header.tag == record::Tag::KITTY_IMAGE_PAYLOAD {
///     let payload = kitty::decode_payload(source, header, &mut targets)?;
///     if payload.key != Key::Primary { return Err(Error::UnexpectedKittyKey) }
/// }
/// ```
pub fn decode_payload(
    source: &mut dyn io::Reader,
    record_header: record::Header,
    targets: &mut PayloadTargets<'_>,
) -> Result<Payload, DecodeError> {
    let mut record_reader = record::Reader::init_header(source, record_header);
    if record_header.tag != record::Tag::KITTY_IMAGE_PAYLOAD {
        return Err(Error::UnexpectedRecordTag);
    }

    let reader = record_reader.payload_reader();
    let key = decode_key(reader)?;
    let image_id = io::read_int::<u32>(reader)?;
    let frame = io::read_int::<u32>(reader)?;
    let decoded_len = io::read_int::<u64>(reader)?;

    let mut result = Payload {
        key,
        image_id,
        frame,
        bytes: 0,
    };

    // Find the pending image or frame these pixels complete, and the
    // ticket to complete it with.
    let ticket: Option<PendingImage> = 'ticket: {
        let Some(t) = &targets[key as usize] else {
            break 'ticket None;
        };
        let Some(img) = t.screen.kitty_images.images.get(&image_id) else {
            break 'ticket None;
        };
        if img.transmission > t.ready_generation {
            break 'ticket None;
        }
        let expected: usize = match frame {
            0 => break 'ticket None,
            1 => match img.data {
                Data::Pending(len) => len,
                Data::Complete(_) => break 'ticket None,
            },
            _ => {
                let Some(anim) = &img.animation else {
                    break 'ticket None;
                };
                if (frame - 2) as usize >= anim.frames.len() {
                    break 'ticket None;
                }
                let pending = &anim.frames[(frame - 2) as usize];
                if !pending.is_pending() {
                    break 'ticket None;
                }
                pending.pending_len
            }
        };
        if expected as u64 != decoded_len {
            break 'ticket None;
        }
        Some(PendingImage {
            id: image_id,
            transmission: img.transmission,
        })
    };
    let Some(pending) = ticket else {
        reader.discard_remaining()?;
        record_reader.finish()?;
        return Ok(result);
    };

    // The record must hold exactly the declared pixels: a short one fails
    // the read, a long one fails `finish`.
    let mut data = vec![0u8; decoded_len as usize];
    reader.read_slice_all(&mut data)?;
    record_reader.finish()?;

    let storage = &mut targets[key as usize].as_mut().unwrap().screen.kitty_images;
    let len = data.len();
    let attached = match frame {
        1 => pending.complete(storage, data),
        _ => pending.complete_frame(storage, frame, data),
    };
    if attached.is_ok() {
        result.bytes = len;
    }
    Ok(result)
}

/// The history page a KITTY_IMAGE_PAGE_PLACEMENTS record restores placements
/// onto. See `decode_page_placements`.
pub struct PageTarget<'a> {
    /// The screen the page belongs to.
    pub screen: &'a mut TerminalScreen,

    /// The history page that was just added to `screen`, which the
    /// record's pins refer to.
    pub page: NodePtr,

    /// The screen's image storage `generation` when the snapshot reached
    /// READY. Generations only grow, so an image whose `transmission` is
    /// newer than this was sent by live input after READY. Such an image
    /// merely shares an ID with the one the snapshot placed, so it gets no
    /// placements from the snapshot.
    pub ready_generation: u64,
}

/// Decode one KITTY_IMAGE_PAGE_PLACEMENTS record, which holds the placements
/// pinned to the history page right before it.
///
/// `record_header` is the record's header, already read from `source`. A
/// header for any other kind of record returns `UnexpectedRecordTag`.
/// See "Decoding Records" in `record.rs`. The record must belong to the
/// screen `key`, which owns the HISTORY sequence it appears in, or this
/// returns `UnexpectedKittyKey`.
///
/// With a `target`, the placements are restored onto that page. Pass null
/// when the page itself was dropped, for example because the terminal was
/// resized since READY. The record is then read and checked, and its
/// placements are dropped along with their page.
///
/// ```ignore
/// // Right after restoring a history page onto the top of `screen`:
/// let header = record::next_header(source)?;
/// if header.tag == record::Tag::KITTY_IMAGE_PAGE_PLACEMENTS {
///     let page = screen.pages.pages.first.unwrap();
///     kitty::decode_page_placements(source, header, Key::Primary, Some(kitty::PageTarget {
///         screen,
///         page,
///         ready_generation: generation_at_ready,
///     }))?;
/// }
/// ```
pub fn decode_page_placements(
    source: &mut dyn io::Reader,
    record_header: record::Header,
    key: TerminalScreenKey,
    mut target: Option<PageTarget<'_>>,
) -> Result<(), DecodeError> {
    let mut record_reader = record::Reader::init_header(source, record_header);
    if record_header.tag != record::Tag::KITTY_IMAGE_PAGE_PLACEMENTS {
        return Err(Error::UnexpectedRecordTag);
    }

    {
        let reader = record_reader.payload_reader();
        if decode_key(reader)? != key {
            return Err(Error::UnexpectedKittyKey);
        }
        let count = io::read_int::<u32>(reader)?;
        for _ in 0..count {
            let decoded = DecodedPlacement::decode(reader, PinContextTag::Page)?;
            let Some(t) = target.as_mut() else {
                continue;
            };
            decoded.restore(t.screen, PinTarget::Page(t.page), Some(t.ready_generation))?;
        }
    }
    record_reader.finish()
}

// ---------------------------------------------------------------------------
// Tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::build_options;
    use crate::terminal::kitty::graphics_storage::TerminalInfo;
    use crate::terminal::page::{Capacity, Cell};
    use crate::terminal::page_list;
    use crate::terminal::render::RenderState;
    use crate::terminal::sgr;
    use crate::terminal::snapshot::envelope;
    use crate::terminal::snapshot::fixture;
    use crate::terminal::snapshot::io::{FixedReader, Limited};
    use crate::terminal::snapshot::snapshot::{self, test_screen_from_pages, DecodeOptions, Decoder};
    use crate::terminal::terminal::{Options as TerminalOptions, Resize, Terminal};
    use base64::Engine as _;

    type Key = TerminalScreenKey;

    /// A small terminal with pixel geometry, so placement sizes are nonzero.
    fn test_terminal(cols: u16, rows: u16) -> Terminal {
        let mut t = Terminal::init(TerminalOptions {
            max_scrollback_bytes: None,
            max_scrollback_lines: None,
            ..TerminalOptions::new(cols, rows)
        })
        .unwrap();
        t.width_px = cols as u32 * 10;
        t.height_px = rows as u32 * 20;
        t
    }

    /// Feed VT bytes to a terminal.
    fn test_feed(t: &mut Terminal, bytes: &[u8]) {
        let mut stream = t.vt_stream();
        stream.next_slice(bytes);
        stream.deinit();
    }

    /// Feed one Kitty graphics command with a base64 encoded payload.
    fn test_command(t: &mut Terminal, keys: &str, payload: &[u8]) {
        let encoded = base64::engine::general_purpose::STANDARD.encode(payload);
        let mut command: Vec<u8> = b"\x1b_G".to_vec();
        command.extend_from_slice(keys.as_bytes());
        if !payload.is_empty() {
            command.push(b';');
        }
        command.extend_from_slice(encoded.as_bytes());
        command.extend_from_slice(b"\x1b\\");
        test_feed(t, &command);
    }

    /// RGBA pixels of one repeated color.
    fn test_pixels(len: usize, rgba: [u8; 4]) -> Vec<u8> {
        let mut result = vec![0u8; len];
        for i in 0..len / 4 {
            result[i * 4..i * 4 + 4].copy_from_slice(&rgba);
        }
        result
    }

    fn test_encode(t: &Terminal) -> Vec<u8> {
        let mut encoded = Vec::new();
        snapshot::encode(
            &mut encoded,
            t,
            snapshot::EncodeOptions {
                continuation: snapshot::Continuation::Ground,
            },
        )
        .unwrap();
        encoded
    }

    /// Decode a complete snapshot through a one-byte reader buffer, so every
    /// record is read in the smallest possible pieces.
    fn test_decode(bytes: &[u8], options: DecodeOptions) -> Result<Terminal, Error> {
        let mut source = FixedReader::new(bytes);
        let mut limited = Limited::new(&mut source, None, 1);
        let mut decoded = snapshot::decode(&mut limited, &options)?;
        Ok(decoded.to_owned())
    }

    const TEST_DECODE_OPTIONS: DecodeOptions = DecodeOptions {
        max_continuation_bytes: 1024,
        compress_history: false,
        kitty_image_storage_limit: None,
    };

    fn screen_of(t: &Terminal, key: Key) -> &TerminalScreen {
        // SAFETY: the screen is owned by `t`.
        unsafe { &*t.screens.get(key).unwrap().as_ptr() }
    }

    /// Require two screens' Kitty graphics storage to hold the same images,
    /// pixels, placements and in-progress load, with the same render geometry.
    fn expect_storage_equal(expected_terminal: &Terminal, actual_terminal: &Terminal, key: Key) {
        let expected_screen = screen_of(expected_terminal, key);
        let actual_screen = screen_of(actual_terminal, key);
        let expected = &expected_screen.kitty_images;
        let actual = &actual_screen.kitty_images;

        assert_eq!(expected.next_image_id, actual.next_image_id);
        assert_eq!(expected.next_internal_placement_id, actual.next_internal_placement_id);
        assert_eq!(expected.total_bytes, actual.total_bytes);

        assert_eq!(expected.images.len(), actual.images.len());
        for e in expected.images.values() {
            let a = actual.images.get(&e.id).unwrap();
            assert_eq!(e.number, a.number);
            assert_eq!(e.width, a.width);
            assert_eq!(e.height, a.height);
            assert_eq!(e.format, a.format);
            assert_eq!(e.metadata, a.metadata);
            assert_eq!(e.data.len(), a.data.len());
            assert_eq!(e.data.is_pending(), a.data.is_pending());
            if let Some(bytes) = e.data.bytes() {
                assert_eq!(bytes, a.data.bytes().unwrap());
            }

            assert_eq!(e.animation.is_none(), a.animation.is_none());
            let Some(e_anim) = &e.animation else {
                continue;
            };
            let a_anim = a.animation.as_ref().unwrap();
            assert_eq!(e_anim.state, a_anim.state);
            assert_eq!(e_anim.max_loops, a_anim.max_loops);
            assert_eq!(e_anim.current_loop, a_anim.current_loop);
            assert_eq!(e_anim.current_index, a_anim.current_index);
            assert_eq!(e_anim.root_gap_ms, a_anim.root_gap_ms);
            assert_eq!(e_anim.frames.len(), a_anim.frames.len());
            for (ef, af) in e_anim.frames.iter().zip(a_anim.frames.iter()) {
                assert_eq!(ef.gap_ms, af.gap_ms);
                assert_eq!(ef.pending_len, af.pending_len);
                assert_eq!(ef.data, af.data);
            }
        }

        let expected_info = TerminalInfo::from_terminal(expected_terminal);
        let actual_info = TerminalInfo::from_terminal(actual_terminal);
        assert_eq!(expected.placements.len(), actual.placements.len());
        for (key, e) in expected.placements.iter() {
            let a = actual.placements.get(key).unwrap();
            assert_eq!(
                std::mem::discriminant(&e.location),
                std::mem::discriminant(&a.location)
            );
            match (e.location, a.location) {
                (PlacementLocation::Pin(e_pin), PlacementLocation::Pin(a_pin)) => {
                    // SAFETY: tracked pins of live placements.
                    let (e_pin, a_pin) = unsafe { (*e_pin.as_ptr(), *a_pin.as_ptr()) };
                    assert_eq!(
                        expected_screen.pages.point_from_pin(point::Tag::Screen, e_pin).unwrap(),
                        actual_screen.pages.point_from_pin(point::Tag::Screen, a_pin).unwrap(),
                    );
                }
                (PlacementLocation::Virtual, _) => {}
                (PlacementLocation::Relative(e_rel), PlacementLocation::Relative(a_rel)) => {
                    assert_eq!(e_rel, a_rel);
                }
                _ => unreachable!(),
            }
            assert_eq!(e.x_offset, a.x_offset);
            assert_eq!(e.y_offset, a.y_offset);
            assert_eq!(e.source_x, a.source_x);
            assert_eq!(e.source_y, a.source_y);
            assert_eq!(e.source_width, a.source_width);
            assert_eq!(e.source_height, a.source_height);
            assert_eq!(e.columns, a.columns);
            assert_eq!(e.rows, a.rows);
            assert_eq!(e.z, a.z);

            // The geometry placement_render_info reports.
            let img = expected.images.get(&key.image_id).unwrap();
            let a_img = actual.images.get(&key.image_id).unwrap();
            assert_eq!(e.pixel_size(img, &expected_info), a.pixel_size(a_img, &actual_info));
            assert_eq!(e.grid_size(img, &expected_info), a.grid_size(a_img, &actual_info));
            assert_eq!(e.source_rect(img), a.source_rect(a_img));
        }

        assert_eq!(expected.loading.is_none(), actual.loading.is_none());
        if let Some(e) = &expected.loading {
            let a = actual.loading.as_ref().unwrap();
            assert_eq!(e.image.id, a.image.id);
            assert_eq!(e.image.number, a.image.number);
            assert_eq!(e.image.width, a.image.width);
            assert_eq!(e.image.height, a.image.height);
            assert_eq!(e.image.format, a.image.format);
            assert_eq!(e.image.compression, a.image.compression);
            assert_eq!(e.image.metadata, a.image.metadata);
            assert_eq!(e.display, a.display);
            assert_eq!(e.frame.is_none(), a.frame.is_none());
            if let Some(f) = &e.frame {
                assert_eq!(f.cmd, a.frame.as_ref().unwrap().cmd);
            }
            assert_eq!(e.quiet, a.quiet);
            assert_eq!(e.response, a.response);
            assert_eq!(e.data, a.data);
        }
    }

    /// Encode `t`, compare it with a golden fixture, decode the checked-in
    /// fixture, and require the same storage contents and an identical
    /// re-encoding.
    fn test_golden(t: &Terminal, name: &str, fixture_bytes: &[u8]) {
        let encoded = test_encode(t);
        fixture::expect_equal(
            fixture::Kind::Snapshot,
            &format!("src/terminal/snapshot/testdata/{name}-v1.hex"),
            &format!("snapshot_fixture-{name}-v1.hex"),
            fixture_bytes,
            &encoded,
        );

        let restored = test_decode(fixture_bytes, TEST_DECODE_OPTIONS).unwrap();
        for key in Key::ALL {
            if t.screens.get(key).is_none() {
                continue;
            }
            expect_storage_equal(t, &restored, key);
        }

        let reencoded = test_encode(&restored);
        assert_eq!(fixture_bytes, &reencoded[..]);
    }

    fn test_placed_fixture() -> Vec<u8> {
        fixture::parse(include_str!("testdata/kitty-placed-v1.hex"))
    }
    fn test_source_rect_fixture() -> Vec<u8> {
        fixture::parse(include_str!("testdata/kitty-source-rect-v1.hex"))
    }
    fn test_virtual_fixture() -> Vec<u8> {
        fixture::parse(include_str!("testdata/kitty-virtual-v1.hex"))
    }
    fn test_relative_fixture() -> Vec<u8> {
        fixture::parse(include_str!("testdata/kitty-relative-v1.hex"))
    }
    fn test_history_fixture() -> Vec<u8> {
        fixture::parse(include_str!("testdata/kitty-history-v1.hex"))
    }
    fn test_animation_fixture() -> Vec<u8> {
        fixture::parse(include_str!("testdata/kitty-animation-v1.hex"))
    }
    fn test_loading_fixture() -> Vec<u8> {
        fixture::parse(include_str!("testdata/kitty-loading-v1.hex"))
    }
    fn test_alternate_fixture() -> Vec<u8> {
        fixture::parse(include_str!("testdata/kitty-alternate-v1.hex"))
    }
    fn test_unknown_fixture() -> Vec<u8> {
        fixture::parse(include_str!("testdata/unknown-records-v1.hex"))
    }
    fn test_payload_in_history_fixture() -> Vec<u8> {
        fixture::parse(include_str!("testdata/kitty-payload-in-history-v1.hex"))
    }

    /// The terminal of the "placed" golden fixture.
    fn test_placed_terminal() -> Terminal {
        let mut t = test_terminal(4, 2);

        // An 8x8 single-color image compresses well.
        let pixels = test_pixels(256, [255, 0, 0, 255]);
        test_command(&mut t, "a=T,f=32,s=8,v=8,i=1,c=2,r=1,C=1,q=2", &pixels);
        assert_eq!(1, t.screens.active.kitty_images.placements.len());
        t
    }

    #[test]
    fn golden_snapshot_with_one_placed_rgba_image() {
        let t = test_placed_terminal();
        test_golden(&t, "kitty-placed", &test_placed_fixture());
    }

    #[test]
    fn golden_snapshot_with_a_source_rectangle_and_offsets() {
        let mut t = test_terminal(4, 2);
        let pixels = test_pixels(64, [0, 255, 0, 255]);
        test_command(
            &mut t,
            "a=T,f=32,s=4,v=4,i=2,p=7,x=1,y=1,w=2,h=2,X=3,Y=4,z=-5,C=1,q=2",
            &pixels,
        );
        test_golden(&t, "kitty-source-rect", &test_source_rect_fixture());
    }

    /// The terminal of the "virtual" golden fixture: a 2x1 virtual placement
    /// drawn by two placeholder cells.
    fn test_virtual_terminal() -> Terminal {
        let mut t = test_terminal(4, 2);
        let pixels = test_pixels(16, [0, 0, 255, 255]);
        test_command(&mut t, "a=T,U=1,f=32,s=2,v=2,i=3,c=2,r=1,q=2", &pixels);
        t.set_attribute(sgr::Attribute::Fg256(3)).unwrap();
        t.print_string("\u{10EEEE}\u{0305}\u{0305}\u{10EEEE}\u{0305}\u{030D}").unwrap();
        t
    }

    #[test]
    fn golden_snapshot_with_a_virtual_placement() {
        let t = test_virtual_terminal();
        test_golden(&t, "kitty-virtual", &test_virtual_fixture());
    }

    #[test]
    fn golden_snapshot_with_a_relative_chain_rooted_at_a_pinned_placement() {
        let mut t = test_terminal(4, 3);
        let pixels = test_pixels(4, [1, 2, 3, 4]);
        test_command(&mut t, "a=t,f=32,s=1,v=1,i=1,q=2", &pixels);
        test_command(&mut t, "a=t,f=32,s=1,v=1,i=2,q=2", &pixels);

        // The root uses an internal placement ID, the children external ones.
        test_command(&mut t, "a=p,i=1,C=1,q=2", b"");
        test_command(&mut t, "a=p,i=2,p=5,P=1,H=1,V=1,q=2", b"");
        test_command(&mut t, "a=p,i=1,p=6,P=2,Q=5,H=-1,V=1,q=2", b"");
        assert_eq!(3, t.screens.active.kitty_images.placements.len());
        test_golden(&t, "kitty-relative", &test_relative_fixture());
    }

    /// A 2x2 terminal whose primary screen has two complete two-row history
    /// pages above the active page, using exact page capacities. The top-left
    /// cells are 'A', 'B' and 'C' from the oldest page.
    fn test_history_terminal() -> Terminal {
        let t = test_terminal(2, 2);

        let mut primary = t.screens.get(Key::Primary).unwrap();
        let mut replacement = {
            let mut builder = page_list::Builder::init(page_list::Options {
                cols: 2,
                rows: 2,
                max_size: None,
                max_lines: None,
            })
            .unwrap();
            for c in ['A', 'B', 'C'] {
                let built = builder.allocate_page(Capacity::new(2, 2)).unwrap();
                built.size.rows = 2;
                // SAFETY: (0, 0) is within the page.
                unsafe { *built.get_row_and_cell(0, 0).cell = Cell::init(c as u32) };
            }
            test_screen_from_pages(builder.finish().unwrap())
        };
        // Keep the storage limit policy of the original screen.
        replacement.kitty_images.total_limit = primary.kitty_images.total_limit;
        *primary = replacement;
        t
    }

    /// Place image `image_id` at a cell of page `node` with a tracked pin.
    fn test_place(t: &mut Terminal, node: NodePtr, image_id: u32, placement_id: u32, y: u16, x: u16) {
        let screen = &mut *t.screens.active;
        let pin = screen
            .pages
            .track_pin(Pin {
                node,
                y,
                x,
                garbage: false,
            })
            .unwrap();
        let TerminalScreen {
            pages,
            kitty_images,
            ..
        } = screen;
        kitty_images
            .add_placement(
                pages,
                image_id,
                placement_id,
                Placement {
                    columns: 1,
                    rows: 1,
                    ..Placement::new(PlacementLocation::Pin(pin))
                },
            )
            .unwrap();
    }

    /// The terminal of the "history" golden fixture: placements on both history
    /// pages and on the active page, plus a relative child of a history
    /// placement.
    fn test_history_placed_terminal() -> Terminal {
        let mut t = test_history_terminal();

        let pixels = test_pixels(4, [9, 9, 9, 255]);
        test_command(&mut t, "a=t,f=32,s=1,v=1,i=4,q=2", &pixels);
        let oldest = t.screens.active.pages.pages.first.unwrap();
        let newest_history = nd(oldest).next.unwrap();
        test_place(&mut t, oldest, 4, 1, 1, 1);
        test_place(&mut t, newest_history, 4, 2, 0, 0);
        test_place(&mut t, nd(newest_history).next.unwrap(), 4, 3, 1, 0);
        test_command(&mut t, "a=p,i=4,p=9,P=4,Q=1,H=1,q=2", b"");
        assert_eq!(4, t.screens.active.kitty_images.placements.len());
        t
    }

    #[test]
    fn golden_snapshot_with_placements_in_history() {
        let t = test_history_placed_terminal();
        test_golden(&t, "kitty-history", &test_history_fixture());
    }

    /// The terminal of the "animation" golden fixture.
    fn test_animation_terminal() -> Terminal {
        let mut t = test_terminal(4, 2);

        test_command(&mut t, "a=T,f=32,s=2,v=1,i=5,C=1,q=2", &test_pixels(8, [255, 0, 0, 255]));
        test_command(&mut t, "a=f,i=5,f=32,s=2,v=1,z=50,q=2", &test_pixels(8, [0, 255, 0, 255]));
        test_command(&mut t, "a=f,i=5,f=32,s=1,v=1,z=70,q=2", &test_pixels(4, [0, 0, 255, 255]));
        test_command(&mut t, "a=a,i=5,r=1,z=30,q=2", b"");
        test_command(&mut t, "a=a,i=5,c=2,q=2", b"");
        test_command(&mut t, "a=a,i=5,s=3,v=3,q=2", b"");
        let anim = t
            .screens
            .active
            .kitty_images
            .image_by_id(5)
            .unwrap()
            .animation
            .as_ref()
            .unwrap();
        assert_eq!(animation::State::Running, anim.state);
        assert_eq!(3, anim.frame_count());
        assert_eq!(1, anim.current_index);
        t
    }

    #[test]
    fn golden_snapshot_with_a_running_three_frame_animation() {
        let t = test_animation_terminal();
        test_golden(&t, "kitty-animation", &test_animation_fixture());
    }

    /// The pixels of the "loading" fixture's chunked transmission, of which
    /// the first `TEST_LOADING_CUT` bytes were sent.
    fn test_loading_pixels() -> Vec<u8> {
        test_pixels(16, [7, 7, 7, 255])
    }
    const TEST_LOADING_CUT: usize = 6;

    #[test]
    fn golden_snapshot_with_an_in_progress_chunked_load() {
        let mut t = test_terminal(4, 2);
        test_command(
            &mut t,
            "a=T,f=32,s=2,v=2,i=6,c=1,r=1,q=2,m=1",
            &test_loading_pixels()[..TEST_LOADING_CUT],
        );
        assert!(t.screens.active.kitty_images.loading.is_some());
        test_golden(&t, "kitty-loading", &test_loading_fixture());
    }

    #[test]
    fn golden_snapshot_with_an_image_on_the_alternate_screen() {
        let mut t = test_terminal(4, 2);
        test_command(&mut t, "a=T,f=32,s=1,v=1,i=1,C=1,q=2", &test_pixels(4, [1, 1, 1, 255]));
        t.switch_screen(Key::Alternate).unwrap();
        test_command(&mut t, "a=T,f=24,s=1,v=1,i=1,C=1,q=2", &[2, 2, 2]);
        test_golden(&t, "kitty-alternate", &test_alternate_fixture());
    }

    /// One framed record within an encoded snapshot.
    #[derive(Debug, Clone, Copy)]
    struct TestRecord {
        start: usize,
        end: usize,
        tag: record::Tag,
    }

    /// Split an encoded snapshot into its records, after the envelope.
    fn test_records(bytes: &[u8]) -> Vec<TestRecord> {
        let mut result = Vec::new();
        let mut offset = envelope::ENCODED_LEN;
        while offset < bytes.len() {
            let tag = u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap());
            let len = u32::from_le_bytes(bytes[offset + 2..offset + 6].try_into().unwrap());
            let end = offset + record::Header::LEN + len as usize;
            result.push(TestRecord {
                start: offset,
                end,
                tag: record::Tag(tag),
            });
            offset = end;
        }
        result
    }

    fn test_is_kitty_tag(tag: record::Tag) -> bool {
        matches!(
            tag,
            record::Tag::KITTY_IMAGE_STORAGE | record::Tag::KITTY_IMAGE_PAYLOAD | record::Tag::KITTY_IMAGE_PAGE_PLACEMENTS
        )
    }

    /// The snapshot without its Kitty graphics records: what a terminal with
    /// the same content but no images encodes to.
    fn test_strip_kitty(bytes: &[u8]) -> Vec<u8> {
        let records = test_records(bytes);
        let mut result = bytes[..records[0].start].to_vec();
        for r in &records {
            if test_is_kitty_tag(r.tag) {
                continue;
            }
            result.extend_from_slice(&bytes[r.start..r.end]);
        }
        result
    }

    /// The snapshot with an unknown record before every record, including
    /// before FINISH, so one appears in every position between records.
    fn test_insert_unknown(bytes: &[u8]) -> Vec<u8> {
        let records = test_records(bytes);
        let mut result: Vec<u8> = bytes[..records[0].start].to_vec();
        for (i, r) in records.iter().enumerate() {
            // Vary the tags and lengths, including an empty payload.
            let payload = [i as u8; 8];
            record::test_write_raw(&mut result, 0x4000 + i as u16, &payload[..i % payload.len()]).unwrap();
            result.extend_from_slice(&bytes[r.start..r.end]);
        }
        result
    }

    #[test]
    fn a_terminal_without_images_encodes_no_kitty_graphics_records() {
        let mut t = test_terminal(4, 2);
        let before = test_encode(&t);
        for r in test_records(&before) {
            assert!(!test_is_kitty_tag(r.tag));
        }

        // Transmitting and deleting an image changes storage counters that
        // are not encoded when the storage is empty again.
        if build_options::KITTY_GRAPHICS {
            test_command(&mut t, "a=t,f=32,s=1,v=1,i=1,q=2", &test_pixels(4, [1, 2, 3, 4]));
            test_command(&mut t, "a=t,f=32,s=1,v=1,I=4,q=2", &test_pixels(4, [1, 2, 3, 4]));
            test_command(&mut t, "a=p,i=1,C=1,q=2", b"");
            assert!(t.screens.active.kitty_images.images.len() == 2);
            test_feed(&mut t, b"\x1b_Ga=d,d=I,i=1,q=2\x1b\\\x1b_Ga=d,d=N,I=4,q=2\x1b\\");
            assert!(t.screens.active.kitty_images.images.is_empty());
            assert!(t.screens.active.kitty_images.next_internal_placement_id != 0);
        }
        let after = test_encode(&t);
        assert_eq!(before, after);
    }

    #[test]
    fn kitty_graphics_records_are_skipped_without_kitty_graphics_support() {
        // Every golden snapshot that carries images.
        let fixtures = [
            test_placed_fixture(),
            test_source_rect_fixture(),
            test_virtual_fixture(),
            test_relative_fixture(),
            test_history_fixture(),
            test_animation_fixture(),
            test_loading_fixture(),
            test_alternate_fixture(),
        ];
        for fixture_bytes in &fixtures {
            let stripped = test_strip_kitty(fixture_bytes);
            assert!(stripped.len() < fixture_bytes.len());

            // With Kitty graphics compiled out, the image records are unknown
            // and the decoded terminal is exactly the one without them. With
            // it, the image-free snapshot is still a valid snapshot of the
            // same terminal content without its images.
            let restored = test_decode(
                if build_options::KITTY_GRAPHICS {
                    &stripped
                } else {
                    fixture_bytes
                },
                TEST_DECODE_OPTIONS,
            )
            .unwrap();
            let reencoded = test_encode(&restored);
            assert_eq!(stripped, reencoded);
        }
    }

    #[test]
    fn golden_snapshot_with_an_unknown_record_in_every_position() {
        let history_fixture = test_history_fixture();
        let unknown_fixture = test_unknown_fixture();

        // The fixture is the history golden, which has Kitty graphics records
        // of every kind, with unknown records interleaved everywhere.
        let expected = test_insert_unknown(&history_fixture);
        fixture::expect_equal(
            fixture::Kind::Snapshot,
            "src/terminal/snapshot/testdata/unknown-records-v1.hex",
            "snapshot_fixture-unknown-records-v1.hex",
            &unknown_fixture,
            &expected,
        );

        // Unknown records are skipped wherever they appear.
        let restored = test_decode(&unknown_fixture, TEST_DECODE_OPTIONS).unwrap();
        let reencoded = test_encode(&restored);
        if build_options::KITTY_GRAPHICS {
            assert_eq!(history_fixture, reencoded);
        } else {
            assert_eq!(test_strip_kitty(&history_fixture), reencoded);
        }

        // The same holds for the incremental decoder.
        {
            let mut source_reader = FixedReader::new(&unknown_fixture);
            let mut decoder = Decoder::init(&mut source_reader);
            let mut decoded = decoder.ready(&TEST_DECODE_OPTIONS).unwrap();
            let mut incremental = decoded.to_owned();
            while decoder.next(&mut incremental).unwrap().is_some() {}
            let incremental_bytes = test_encode(&incremental);
            assert_eq!(reencoded, incremental_bytes);
        }

        // A corrupted unknown record still fails its CRC, in every position.
        for r in test_records(&unknown_fixture) {
            if r.tag.known() {
                continue;
            }
            let mut corrupted = unknown_fixture.clone();
            corrupted[r.start] ^= 0x01;
            assert_eq!(
                Err(Error::InvalidChecksum),
                test_decode(&corrupted, TEST_DECODE_OPTIONS).map(|_| ())
            );
        }
    }

    #[test]
    fn decode_with_a_smaller_limit_keeps_the_newest_images() {
        let mut t = test_terminal(4, 3);

        // Three 64-byte images, oldest first. The oldest has a placement with
        // a relative child on another image.
        let pixels = test_pixels(64, [5, 5, 5, 255]);
        test_command(&mut t, "a=T,f=32,s=4,v=4,i=1,p=1,C=1,q=2", &pixels);
        test_command(&mut t, "a=T,f=32,s=4,v=4,i=2,p=1,C=1,q=2", &pixels);
        test_command(&mut t, "a=T,f=32,s=4,v=4,i=3,p=1,C=1,q=2", &pixels);
        test_command(&mut t, "a=p,i=3,p=2,P=1,Q=1,H=1,q=2", b"");
        assert_eq!(4, t.screens.active.kitty_images.placements.len());
        let encoded = test_encode(&t);

        let restored = test_decode(
            &encoded,
            DecodeOptions {
                max_continuation_bytes: 0,
                kitty_image_storage_limit: Some(150),
                ..Default::default()
            },
        )
        .unwrap();
        let storage = &restored.screens.active.kitty_images;
        assert_eq!(150, storage.total_limit);
        assert_eq!(2, storage.images.len());
        assert!(storage.image_by_id(1).is_none());
        assert_eq!(&pixels[..], storage.image_by_id(2).unwrap().data.bytes().unwrap());
        assert_eq!(&pixels[..], storage.image_by_id(3).unwrap().data.bytes().unwrap());

        // The placement of the dropped image and its relative child are gone.
        assert_eq!(2, storage.placements.len());
        for key in storage.placements.keys() {
            assert!(key.image_id != 1);
        }
        assert_eq!(128, storage.total_bytes);

        // A limit too small for any image keeps none. The pixels for those
        // images are then skipped without an error.
        let empty = test_decode(
            &encoded,
            DecodeOptions {
                max_continuation_bytes: 0,
                kitty_image_storage_limit: Some(10),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(0, empty.screens.active.kitty_images.images.len());
        assert_eq!(0, empty.screens.active.kitty_images.total_bytes);
    }

    #[test]
    fn incremental_decode_restores_images_at_ready_and_payloads_after() {
        let mut t = test_animation_terminal();
        test_command(&mut t, "a=T,f=32,s=8,v=8,i=9,C=1,q=2", &test_pixels(256, [3, 3, 3, 255]));
        let encoded = test_encode(&t);

        let mut source = FixedReader::new(&encoded);
        let mut decoder = Decoder::init(&mut source);
        let mut decoded = decoder.ready(&TEST_DECODE_OPTIONS).unwrap();
        let mut restored = decoded.to_owned();

        // At READY every image and placement exists, with pending pixels
        // reserved against the limit, and the animation does not play.
        let counts = decoded.kitty[Key::Primary as usize];
        assert_eq!(2, counts.images);
        assert_eq!(256 + 3 * 8, counts.pending_bytes);
        assert_eq!(0, decoded.kitty[Key::Alternate as usize].images);
        {
            let storage = &mut restored.screens.active.kitty_images;
            assert_eq!(t.screens.active.kitty_images.total_bytes, storage.total_bytes);
            assert!(storage.image_by_id(5).unwrap().data.is_pending());
            assert!(storage.image_by_id(5).unwrap().render_data().is_pending());
            assert!(storage.animation_tick(0).is_none());
            assert_eq!(2, storage.placements.len());
        }

        // Payloads arrive one per call: the displayed frame of every visible
        // image first, oldest image first, then the other animation frames.
        let expected: [(u32, u32, usize); 4] = [(5, 2, 8), (9, 1, 256), (5, 1, 8), (5, 3, 8)];
        for (i, (id, frame, bytes)) in expected.iter().copied().enumerate() {
            let progress = decoder.next(&mut restored).unwrap().unwrap();
            assert_eq!(
                snapshot::Progress::Image(snapshot::ProgressImage {
                    key: Key::Primary,
                    image_id: id,
                    frame,
                    bytes,
                }),
                progress
            );

            // The animation waits for its last frame.
            let playing = restored.screens.active.kitty_images.animation_tick(1000).is_some();
            assert_eq!(i == expected.len() - 1, playing);
        }
        assert!(decoder.next(&mut restored).unwrap().is_none());
        expect_storage_equal(&t, &restored, Key::Primary);
    }

    #[test]
    fn a_payload_for_an_image_replaced_after_ready_is_dropped() {
        let t = test_placed_terminal();
        let encoded = test_encode(&t);

        let mut source = FixedReader::new(&encoded);
        let mut decoder = Decoder::init(&mut source);
        let mut decoded = decoder.ready(&TEST_DECODE_OPTIONS).unwrap();
        let mut restored = decoded.to_owned();

        // Live input sends a new image with the same ID before the payload
        // arrives. The payload's pixels belong to the old image.
        let live = test_pixels(4, [0, 0, 0, 255]);
        test_command(&mut restored, "a=t,f=32,s=1,v=1,i=1,q=2", &live);

        let progress = decoder.next(&mut restored).unwrap().unwrap().image();
        assert_eq!(1, progress.image_id);
        assert_eq!(0, progress.bytes);
        assert!(decoder.next(&mut restored).unwrap().is_none());

        let storage = &restored.screens.active.kitty_images;
        assert_eq!(&live[..], storage.image_by_id(1).unwrap().data.bytes().unwrap());
        assert_eq!(4, storage.total_bytes);
    }

    #[test]
    fn a_dropped_history_page_drops_its_placements() {
        let t = test_history_placed_terminal();
        let encoded = test_encode(&t);

        // Without changes, the incremental decoder restores every history
        // placement with its page.
        {
            let mut source = FixedReader::new(&encoded);
            let mut decoder = Decoder::init(&mut source);
            let mut decoded = decoder.ready(&TEST_DECODE_OPTIONS).unwrap();
            let restored = decoded.terminal.as_mut().unwrap();
            assert_eq!(1, restored.screens.active.kitty_images.placements.len());

            // The payload comes first, then each history page with its
            // placements.
            let _ = decoder.next(restored).unwrap().unwrap().image();
            let newest = decoder.next(restored).unwrap().unwrap().page();
            assert_eq!(2, newest.rows);
            assert_eq!(2, restored.screens.active.kitty_images.placements.len());
            let oldest = decoder.next(restored).unwrap().unwrap().page();
            assert_eq!(2, oldest.rows);
            assert_eq!(4, restored.screens.active.kitty_images.placements.len());
            assert!(decoder.next(restored).unwrap().is_none());
            expect_storage_equal(&t, restored, Key::Primary);
        }

        // After a resize, history pages and their placement records are read
        // but dropped, and FINISH still validates.
        {
            let mut source = FixedReader::new(&encoded);
            let mut decoder = Decoder::init(&mut source);
            let mut decoded = decoder.ready(&TEST_DECODE_OPTIONS).unwrap();
            let mut restored = decoded.to_owned();
            restored.resize(Resize::new(3, 2)).unwrap();

            let mut pages: usize = 0;
            while let Some(progress) = decoder.next(&mut restored).unwrap() {
                let page = match progress {
                    snapshot::Progress::Page(page) => page,
                    snapshot::Progress::Image(_) => continue,
                };
                assert_eq!(0, page.rows);
                pages += 1;
            }
            assert_eq!(2, pages);
            let storage = &restored.screens.active.kitty_images;
            for key in storage.placements.keys() {
                assert_eq!(
                    PlacementId {
                        tag: PlacementTag::External,
                        id: 3
                    },
                    key.placement_id
                );
            }
        }
    }

    #[test]
    fn an_image_placed_only_in_history_arrives_with_its_page() {
        let mut t = test_history_terminal();

        // One image on the oldest history page, and one that is not placed.
        test_command(&mut t, "a=t,f=32,s=1,v=1,i=4,q=2", &test_pixels(4, [9, 9, 9, 255]));
        test_command(&mut t, "a=t,f=32,s=1,v=1,i=5,q=2", &test_pixels(4, [7, 7, 7, 255]));
        let oldest = t.screens.active.pages.pages.first.unwrap();
        test_place(&mut t, oldest, 4, 1, 1, 1);
        let encoded = test_encode(&t);

        // The placed image's pixels follow its page's placements, and the
        // unplaced image's pixels come after all history.
        let mut tags: Vec<record::Tag> = Vec::new();
        let mut after_ready = false;
        for r in test_records(&encoded) {
            if after_ready {
                tags.push(r.tag);
            }
            if r.tag == record::Tag::READY {
                after_ready = true;
            }
        }
        assert_eq!(
            vec![
                record::Tag::HISTORY,
                record::Tag::PAGE,
                record::Tag::PAGE,
                record::Tag::KITTY_IMAGE_PAGE_PLACEMENTS,
                record::Tag::KITTY_IMAGE_PAYLOAD,
                record::Tag::KITTY_IMAGE_PAYLOAD,
                record::Tag::FINISH,
            ],
            tags
        );

        // The incremental decoder sees the same order, and the placement is
        // there by the time its pixels arrive.
        let mut source = FixedReader::new(&encoded);
        let mut decoder = Decoder::init(&mut source);
        let mut decoded = decoder.ready(&TEST_DECODE_OPTIONS).unwrap();
        let restored = decoded.terminal.as_mut().unwrap();
        assert_eq!(0, restored.screens.active.kitty_images.placements.len());
        for _ in 0..2 {
            let _ = decoder.next(restored).unwrap().unwrap().page();
        }
        assert_eq!(1, restored.screens.active.kitty_images.placements.len());
        assert!(restored
            .screens
            .active
            .kitty_images
            .image_by_id(4)
            .unwrap()
            .data
            .is_pending());
        let first = decoder.next(restored).unwrap().unwrap().image();
        assert_eq!(4, first.image_id);
        assert_eq!(4, first.bytes);
        let second = decoder.next(restored).unwrap().unwrap().image();
        assert_eq!(5, second.image_id);
        assert!(decoder.next(restored).unwrap().is_none());
        expect_storage_equal(&t, restored, Key::Primary);
    }

    #[test]
    fn history_placements_restore_onto_compressed_history_pages() {
        let t = test_history_placed_terminal();
        let encoded = test_encode(&t);

        let restored = test_decode(
            &encoded,
            DecodeOptions {
                max_continuation_bytes: 1024,
                compress_history: true,
                ..Default::default()
            },
        )
        .unwrap();
        expect_storage_equal(&t, &restored, Key::Primary);
        assert_eq!(4, restored.screens.active.kitty_images.placements.len());
    }

    #[test]
    fn a_restored_in_progress_load_continues_the_chunked_transmission() {
        let mut t = test_terminal(4, 2);
        let pixels = test_loading_pixels();
        test_command(&mut t, "a=T,f=32,s=2,v=2,i=6,c=1,r=1,q=2,m=1", &pixels[..TEST_LOADING_CUT]);

        let mut restored = test_decode(&test_loading_fixture(), TEST_DECODE_OPTIONS).unwrap();

        // The rest of the transmission completes and displays the image on
        // both terminals alike.
        for target in [&mut t, &mut restored] {
            test_command(target, "m=0", &pixels[TEST_LOADING_CUT..]);
            let storage = &target.screens.active.kitty_images;
            assert!(storage.loading.is_none());
            assert_eq!(&pixels[..], storage.image_by_id(6).unwrap().data.bytes().unwrap());
            assert_eq!(1, storage.placements.len());
        }
        expect_storage_equal(&t, &restored, Key::Primary);
    }

    #[test]
    fn a_payload_with_fewer_pixels_than_it_declares_fails() {
        let placed = test_placed_fixture();
        let payload = test_records(&placed)
            .into_iter()
            .find(|r| r.tag == record::Tag::KITTY_IMAGE_PAYLOAD)
            .unwrap();

        // Drop the last pixel byte and fix up the record's length and CRC, so
        // the record is well formed but shorter than its declared pixels.
        let mut short: Vec<u8> = placed[..payload.start].to_vec();
        let body = &placed[payload.start + record::Header::LEN..payload.end - 1];
        record::test_write_raw(&mut short, record::Tag::KITTY_IMAGE_PAYLOAD.0, body).unwrap();
        short.extend_from_slice(&placed[payload.end..]);

        assert_eq!(
            Err(Error::EndOfStream),
            test_decode(&short, TEST_DECODE_OPTIONS).map(|_| ())
        );
    }

    #[test]
    fn completing_a_placeholder_images_payload_updates_the_render_state() {
        let t = test_virtual_terminal();
        let encoded = test_encode(&t);

        let mut source = FixedReader::new(&encoded);
        let mut decoder = Decoder::init(&mut source);
        let mut decoded = decoder.ready(&TEST_DECODE_OPTIONS).unwrap();
        let mut restored = decoded.to_owned();

        let mut state = RenderState::empty();

        // The pending image draws nothing yet, even though its placeholder
        // characters are on screen.
        state.update(&mut restored);
        assert!(state.image_placement_iterator().next().is_none());

        // The payload completes the image and changes the storage generation,
        // so the next update looks at the placeholders again and finds the
        // image.
        let generation = restored.screens.active.kitty_images.generation;
        let progress = decoder.next(&mut restored).unwrap().unwrap().image();
        assert_eq!(16, progress.bytes);
        assert!(restored.screens.active.kitty_images.generation != generation);
        state.update(&mut restored);
        let mut it = state.image_placement_iterator();
        let placement = it.next().unwrap();
        assert_eq!(3, placement.image_id);
        assert_eq!(2, placement.cols);
        assert!(it.next().is_none());
    }

    /// `bytes` with its one KITTY_IMAGE_PAYLOAD record moved to just before the
    /// record at `position`, counted among the other records.
    fn test_move_payload(bytes: &[u8], position: usize) -> Vec<u8> {
        let records = test_records(bytes);
        let payload = *records
            .iter()
            .find(|r| r.tag == record::Tag::KITTY_IMAGE_PAYLOAD)
            .unwrap();

        let mut result: Vec<u8> = bytes[..records[0].start].to_vec();
        let mut index: usize = 0;
        for r in &records {
            if r.tag == record::Tag::KITTY_IMAGE_PAYLOAD {
                continue;
            }
            if index == position {
                result.extend_from_slice(&bytes[payload.start..payload.end]);
            }
            result.extend_from_slice(&bytes[r.start..r.end]);
            index += 1;
        }
        result
    }

    #[test]
    fn a_payload_may_come_anywhere_after_ready() {
        let t = test_history_placed_terminal();
        let encoded = test_encode(&t);

        // The positions of READY and FINISH among the records other than the
        // payload.
        let others: Vec<record::Tag> = test_records(&encoded)
            .into_iter()
            .filter(|r| r.tag != record::Tag::KITTY_IMAGE_PAYLOAD)
            .map(|r| r.tag)
            .collect();
        let ready = others.iter().position(|&tag| tag == record::Tag::READY).unwrap();
        let finish = others.len() - 1;
        assert_eq!(record::Tag::FINISH, others[finish]);

        // Try the payload in front of every record after READY, up to and
        // including FINISH. Each restores the same terminal, except a payload
        // between a PAGE and its KITTY_IMAGE_PAGE_PLACEMENTS, which is not
        // allowed.
        for position in ready + 1..finish + 1 {
            let moved = test_move_payload(&encoded, position);
            if others[position] == record::Tag::KITTY_IMAGE_PAGE_PLACEMENTS {
                assert_eq!(
                    Err(Error::UnexpectedRecordTag),
                    test_decode(&moved, TEST_DECODE_OPTIONS).map(|_| ())
                );
                continue;
            }

            let restored = test_decode(&moved, TEST_DECODE_OPTIONS).unwrap();
            expect_storage_equal(&t, &restored, Key::Primary);
        }

        // A golden fixture with the payload between two history pages, so the
        // schema check sees a payload inside HISTORY too.
        let index = others
            .iter()
            .enumerate()
            .position(|(i, &tag)| {
                i > ready && tag == record::Tag::PAGE && others[i - 1] == record::Tag::KITTY_IMAGE_PAGE_PLACEMENTS
            })
            .unwrap();
        let expected = test_move_payload(&encoded, index);
        fixture::expect_equal(
            fixture::Kind::Snapshot,
            "src/terminal/snapshot/testdata/kitty-payload-in-history-v1.hex",
            "snapshot_fixture-kitty-payload-in-history-v1.hex",
            &test_payload_in_history_fixture(),
            &expected,
        );
    }

    #[test]
    fn kitty_image_storage_must_belong_to_the_screen_before_it() {
        let t = test_placed_terminal();
        let primary = screen_of(&t, Key::Primary);

        // Write the primary screen's storage, but name the alternate screen.
        let mut encoded: Vec<u8> = Vec::new();
        {
            let mut stream = record::Writer::init(&mut encoded);
            envelope::encode(stream.writer()).unwrap();
            crate::terminal::snapshot::terminal::encode(&t, &mut stream).unwrap();
            crate::terminal::snapshot::screen::encode(primary, Key::Primary, &mut stream).unwrap();
            let plan = ScreenPlan::init(primary, Key::Alternate).unwrap();
            plan.encode_storage(&mut stream).unwrap();
            crate::terminal::snapshot::continuation::encode(
                crate::terminal::snapshot::continuation::Value::Ground,
                &mut stream,
            )
            .unwrap();
        }

        assert_eq!(
            Err(Error::UnexpectedKittyKey),
            test_decode(&encoded, TEST_DECODE_OPTIONS).map(|_| ())
        );
    }

    #[test]
    fn history_placements_skip_an_image_replaced_after_ready() {
        let t = test_history_placed_terminal();
        let encoded = test_encode(&t);

        let mut source = FixedReader::new(&encoded);
        let mut decoder = Decoder::init(&mut source);
        let mut decoded = decoder.ready(&TEST_DECODE_OPTIONS).unwrap();
        let mut restored = decoded.to_owned();

        // Live input sends a new image with the same ID before the history
        // pages arrive. It is a different image, so it gets none of the
        // snapshot's history placements.
        let live = test_pixels(4, [0, 0, 0, 255]);
        test_command(&mut restored, "a=t,f=32,s=1,v=1,i=4,q=2", &live);
        while decoder.next(&mut restored).unwrap().is_some() {}

        let storage = &restored.screens.active.kitty_images;
        assert_eq!(&live[..], storage.image_by_id(4).unwrap().data.bytes().unwrap());
        assert_eq!(0, storage.placements.len());
    }
}
