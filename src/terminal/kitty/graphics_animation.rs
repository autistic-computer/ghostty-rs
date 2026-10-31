//! Port of `src/terminal/kitty/graphics_animation.zig`.
//!
//! STATUS: ported
//!
//! Kitty graphics protocol animation support.
//!
//! https://sw.kovidgoyal.net/kitty/graphics-protocol/#animation
//!
//! An animation is a set of frames attached to an existing image.
//! Frame 1 (the "root frame") is the image's own base data. Frames
//! 2..N are stored here. Unlike Kitty, which stores frames as delta
//! rectangles in a disk cache and composes them lazily, we compose
//! every frame eagerly into a full image-sized RGBA buffer at load
//! time. This trades some memory for a much simpler model: a frame is
//! always ready to display and there are no reference chains to
//! maintain, coalesce, or garbage collect. Frame storage is bounded
//! by the same byte limit as image storage.
//!
//! To keep composition to a single pixel format, an image is
//! converted to RGBA the first time an animation command composes
//! into it, and all transmitted frame data is converted to RGBA
//! before composition. Blending an opaque (alpha=255) source pixel
//! degenerates to a copy, so this loses no information relative to
//! Kitty's separate RGB/RGBA paths. The pixel-level primitives
//! (conversion, fill, rectangle composition) live in
//! graphics_pixel.zig.

/// The gap assigned to a newly created frame when the command doesn't
/// specify one (z omitted or z=0). Taken from Kitty (DEFAULT_GAP).
pub const DEFAULT_GAP_MS: u32 = 40;

/// Animation state for a single image. Heap-allocated and owned by
/// the Image. This is created lazily by the first animation command that
/// needs it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Animation {
    /// The frames following the root frame: `frames[i]` is
    /// protocol frame number `i + 2`. The root frame (frame 1) is the
    /// image's base data and is not stored here.
    pub frames: Vec<Frame>,

    /// The gap of the root frame in milliseconds. Zero means gapless:
    /// the frame is skipped during playback. Kitty creates the root
    /// frame with a zero gap; the client gives it one with a=a,r=1,z=N.
    pub root_gap_ms: u32,

    /// The zero-based index of the frame currently displayed: 0 is
    /// the root frame, i >= 1 is frames[i - 1].
    pub current_index: u32,

    /// Playback state. Every image starts stopped (client-driven).
    pub state: State,

    /// Maximum number of loops to play (0 = infinite). Set from the
    /// a=a v key as v-1, per the protocol's off-by-one encoding.
    pub max_loops: u32,

    /// Number of completed loops since playback started.
    pub current_loop: u32,

    /// Timestamp in milliseconds (on the ticker's clock, see
    /// ImageStorage.animationTick) when the current frame was shown.
    /// None means "not yet shown"; the next tick stamps it.
    pub frame_shown_at_ms: Option<u64>,
}

/// `Animation.State`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum State {
    /// Not advancing; frames change only via a=a c=N (client-driven).
    #[default]
    Stopped,
    /// Advancing, but playback parks on the last frame waiting
    /// for more frames instead of looping (a=a s=2).
    Loading,
    /// Advancing and looping (a=a s=3).
    Running,
}

/// One frame after the root frame (`Animation.Frame`).
///
/// A frame is normally complete: `data` holds its pixels. A frame can
/// also be pending, which means it exists and has its place in the
/// animation, but its pixels have not arrived yet. This happens when
/// a terminal is restored from a snapshot, where the frame list
/// arrives first and the pixels follow later. A pending frame keeps
/// its full size reserved against the storage limit, so the pixels
/// always fit when they arrive.
///
/// ```ignore
/// // A pending frame for a 2x2 image: 2 * 2 * 4 bytes of RGBA.
/// let frame = Frame { data: Vec::new(), gap_ms: 40, pending_len: 16 };
/// frame.is_pending(); // true
/// frame.storage_size(); // 16
/// ```
///
/// Use `ImageStorage.PendingImage.completeFrame` to attach the pixels
/// to a pending frame.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Frame {
    /// Fully composed pixel data, always image width * height * 4
    /// bytes of RGBA. Empty while the frame is pending.
    pub data: Vec<u8>,

    /// Milliseconds this frame is displayed before advancing.
    /// Zero means gapless: skipped during playback.
    pub gap_ms: u32,

    /// The byte length of the pixels this frame is waiting for, or
    /// zero if the frame is complete. Always image width * height * 4
    /// for a pending frame.
    pub pending_len: usize,
}

impl Frame {
    /// Returns true if this frame's pixels have not arrived yet.
    pub fn is_pending(&self) -> bool {
        self.pending_len > 0
    }

    /// Bytes of pixel data this frame holds, or will hold once its
    /// pixels arrive. This is what the frame counts against the
    /// storage limit.
    pub fn storage_size(&self) -> usize {
        if self.is_pending() {
            self.pending_len
        } else {
            self.data.len()
        }
    }
}

impl Animation {
    /// The total number of frames including the root frame.
    pub fn frame_count(&self) -> u32 {
        (self.frames.len() + 1) as u32
    }

    /// The gap of the frame at the given zero-based index.
    pub fn gap_at(&self, index: u32) -> u32 {
        if index == 0 {
            return self.root_gap_ms;
        }
        self.frames[index as usize - 1].gap_ms
    }

    /// Set the gap of the frame at the given zero-based index.
    pub fn set_gap_at(&mut self, index: u32, gap_ms: u32) {
        if index == 0 {
            self.root_gap_ms = gap_ms;
        } else {
            self.frames[index as usize - 1].gap_ms = gap_ms;
        }
    }

    /// The sum of all frame gaps. An animation with a zero duration
    /// (every frame gapless) never advances; this doubles as the
    /// guard that keeps the gapless-skip loop in animationTick from
    /// spinning forever, exactly like Kitty's animation_duration.
    pub fn duration_ms(&self) -> u64 {
        let mut total: u64 = self.root_gap_ms as u64;
        for frame in &self.frames {
            total += frame.gap_ms as u64;
        }
        total
    }

    /// Bytes of frame data held by this animation, counted against
    /// the image storage limit. Pending frames count at their full size.
    pub fn frame_bytes(&self) -> usize {
        self.frames.iter().map(Frame::storage_size).sum()
    }

    /// Returns true if any frame's pixels have not arrived yet. An
    /// animation with pending frames does not play until they all
    /// arrive, the same as an image whose own pixels are pending.
    pub fn has_pending_frames(&self) -> bool {
        self.frames.iter().any(Frame::is_pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn animation_gap_helpers() {
        let mut anim = Animation::default();
        anim.frames.push(Frame {
            data: vec![0; 4],
            gap_ms: 100,
            pending_len: 0,
        });

        assert_eq!(2u32, anim.frame_count());
        assert_eq!(0u32, anim.gap_at(0));
        assert_eq!(100u32, anim.gap_at(1));
        assert_eq!(100u64, anim.duration_ms());
        assert_eq!(4usize, anim.frame_bytes());

        anim.set_gap_at(0, 40);
        anim.set_gap_at(1, 60);
        assert_eq!(100u64, anim.duration_ms());
        assert_eq!(40u32, anim.root_gap_ms);
    }

    #[test]
    fn pending_frame() {
        // A pending frame for a 2x2 image: 2 * 2 * 4 bytes of RGBA.
        let frame = Frame {
            data: Vec::new(),
            gap_ms: 40,
            pending_len: 16,
        };
        assert!(frame.is_pending());
        assert_eq!(16, frame.storage_size());

        let mut anim = Animation::default();
        assert!(!anim.has_pending_frames());
        anim.frames.push(frame);
        assert!(anim.has_pending_frames());
        assert_eq!(16, anim.frame_bytes());
    }
}
