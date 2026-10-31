//! Port of `src/apprt/gtk/winproto/BlurRegion.zig`.
//!
//! STATUS: ported

use std::fmt;
use std::os::raw::c_long;

use gtk4::gdk;
use gtk4::gdk::prelude::*;
use gtk4::prelude::*;

use super::ApprtWindow;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlurRegion {
    pub slices: Vec<Slice>,
}

/// A rectangular slice of the blur region.
// Marked `extern` since we want to be able to use this in X11 directly.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slice {
    pub x: Pos,
    pub y: Pos,
    pub width: Pos,
    pub height: Pos,
}

// X11 compatibility. Ideally this should just be an `i32` like Wayland,
// but XLib sucks
pub type Pos = c_long;

impl fmt::Display for BlurRegion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.slices)
    }
}

impl BlurRegion {
    pub const EMPTY: BlurRegion = BlurRegion { slices: Vec::new() };

    pub fn deinit(&mut self) {
        self.slices = Vec::new();
    }

    // Calculate the blur regions for a window.
    //
    // Since we have rounded corners by default, we need to carve out the
    // pixels on each corner to avoid the "korners bug".
    // (cf. https://github.com/cutefishos/fishui/blob/41d4ba194063a3c7fff4675619b57e6ac0504f06/src/platforms/linux/blurhelper/windowblur.cpp#L134)
    pub fn calc_for_window(
        window: &dyn ApprtWindow,
        csd: bool,
        to_device_coordinates: bool,
    ) -> BlurRegion {
        let gtk_window = window.window();
        let native = gtk_window.upcast_ref::<gtk4::Native>();
        let Some(surface) = native.surface() else {
            return BlurRegion::EMPTY;
        };

        // Calculate the primary blur region
        // (the one that covers most of the screen).
        // It's easier to do this inside a vector since we have to scale
        // everything by the scale factor anyways.

        // NOTE(pluiedev): CSDs are a f--king mistake.
        // Please, GNOME, stop this nonsense of making a window ~30% bigger
        // internally than how they really are just for your shadows and
        // rounded corners and all that fluff. Please. I beg of you.
        let (x, y): (Pos, Pos) = {
            let (mut x, mut y) = native.surface_transform();
            // Slightly inset the corners if we're using CSDs
            if csd {
                x += 1.0;
                y += 1.0;
            }
            (x as Pos, y as Pos)
        };

        let mut width = surface.width() as Pos;
        let mut height = surface.height() as Pos;

        // Trim off the offsets. Be careful not to get negative.
        width -= x * 2;
        height -= y * 2;
        if width <= 0 || height <= 0 {
            return BlurRegion::EMPTY;
        }

        // Empirically determined.
        let are_corners_rounded = 'rounded: {
            // This cast should always succeed as all of our windows
            // should be toplevel. If this fails, something very strange
            // is going on.
            let Some(toplevel) = surface.dynamic_cast_ref::<gdk::Toplevel>() else {
                break 'rounded false;
            };

            let state = toplevel.state();
            if state.intersects(
                gdk::ToplevelState::FULLSCREEN
                    | gdk::ToplevelState::MAXIMIZED
                    | gdk::ToplevelState::TILED,
            ) {
                break 'rounded false;
            }

            csd
        };

        let mut new_slices = approx_rounded_rect(
            x,
            y,
            width,
            height,
            // See https://gnome.pages.gitlab.gnome.org/libadwaita/doc/main/css-variables.html#window-radius
            if are_corners_rounded { 15 } else { 0 },
        );

        if to_device_coordinates {
            // Transform surface coordinates to device coordinates.
            let sf = surface.scale_factor() as Pos;
            for s in new_slices.iter_mut() {
                s.x *= sf;
                s.y *= sf;
                s.width *= sf;
                s.height *= sf;
            }
        }

        BlurRegion { slices: new_slices }
    }

    /// Whether two sets of blur regions are equal.
    pub fn eql(&self, other: &BlurRegion) -> bool {
        self.slices == other.slices
    }
}

/// Approximate a rounded rectangle with many smaller rectangles.
fn approx_rounded_rect(x: Pos, y: Pos, width: Pos, height: Pos, radius: Pos) -> Vec<Slice> {
    let r_f = radius as f32;

    let mut slices = Vec::new();

    // Add the central rectangle
    slices.push(Slice {
        x,
        y: y + radius,
        width,
        height: height - 2 * radius,
    });

    // Add the corner rows. This is honestly quite cursed.
    let mut row: Pos = 0;
    while row < radius {
        // y distance from this row to the center corner circle
        let dy = (radius - row) as f32 - 0.5;

        // x distance - as given by the definition of a circle
        let dx = (r_f * r_f - dy * dy).sqrt();

        // How much each row should be offset, rounded to an integer
        let row_x = (r_f - (dx + 0.5).round()) as Pos;

        // Remove the offset from both ends
        let row_w = width - 2 * row_x;

        // Top slice
        slices.push(Slice {
            x: x + row_x,
            y: y + row,
            width: row_w,
            height: 1,
        });

        // Bottom slice
        slices.push(Slice {
            x: x + row_x,
            y: y + height - 1 - row,
            width: row_w,
            height: 1,
        });

        row += 1;
    }

    slices
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slice_is_x11_compatible() {
        assert_eq!(std::mem::size_of::<Slice>(), 4 * std::mem::size_of::<c_long>());
    }

    #[test]
    fn square_rect_is_one_slice() {
        let slices = approx_rounded_rect(1, 2, 100, 50, 0);
        assert_eq!(slices, vec![Slice { x: 1, y: 2, width: 100, height: 50 }]);
    }

    #[test]
    fn rounded_rect_carves_corners() {
        let slices = approx_rounded_rect(0, 0, 100, 100, 15);
        assert_eq!(slices.len(), 1 + 2 * 15);
        assert_eq!(slices[0], Slice { x: 0, y: 15, width: 100, height: 70 });
        // The outermost rows are inset the most, the innermost not at all.
        let top_first = slices[1];
        let top_last = slices[slices.len() - 2];
        assert!(top_first.x > top_last.x);
        assert_eq!(top_first.y, 0);
        assert_eq!(slices[2].y, 99);
        assert_eq!(top_last.y, 14);
        for s in &slices[1..] {
            assert_eq!(s.width, 100 - 2 * s.x);
            assert_eq!(s.height, 1);
        }
        let region = BlurRegion { slices: slices.clone() };
        assert!(region.eql(&BlurRegion { slices }));
        assert!(!region.eql(&BlurRegion::EMPTY));
    }
}
