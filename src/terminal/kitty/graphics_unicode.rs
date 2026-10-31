//! Port of `src/terminal/kitty/graphics_unicode.zig`.
//!
//! STATUS: ported
//!
//! This file contains various logic and data for working with the
//! Kitty graphics protocol unicode placeholder, virtual placement feature.

use std::fmt;

use crate::terminal::kitty::graphics_image::Image;
use crate::terminal::kitty::graphics_render::Placement as RenderPlacement;
use crate::terminal::kitty::graphics_storage::Placement as StoragePlacement;
use crate::terminal::page_list::Pin;
use crate::terminal::size::CellCountInt;
use crate::terminal::style::{Color, Style};

const LOG_TARGET: &str = "kitty_gfx";

/// Codepoint for the unicode placeholder character.
pub const PLACEHOLDER: u32 = 0x10EEEE;

/// A virtual placement in the terminal. This can represent more than
/// one cell if the cells combine to be a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    /// The top-left pin of the placement. This can be used to get the
    /// screen x/y.
    pub pin: Pin,

    /// The image ID and placement ID for this virtual placement. The
    /// image ID is encoded in the fg color (plus optional a 8-bit high
    /// value in the 3rd diacritic). The placement ID is encoded in the
    /// underline color (optionally).
    pub image_id: u32,
    pub placement_id: u32,

    /// Starting row/col index for the image itself. This is the "fragment"
    /// of the image we want to show in this placement. This is 0-indexed.
    pub col: u32,
    pub row: u32,

    /// The width/height in cells of this placement.
    pub width: u32,
    pub height: u32,
}

/// `Placement.Error`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlacementError {
    PlacementGridOutOfBounds,
}

impl fmt::Display for PlacementError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for PlacementError {}

/// The grid size computed by [`Placement::grid`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Grid {
    rows: u32,
    columns: u32,
}

impl Placement {
    /// Take this virtual placement and convert it to a render placement.
    /// `target` is the virtual placement that the cells show, from
    /// `ImageStorage.placeholderTarget`.
    pub fn render_placement(
        &self,
        target: &StoragePlacement,
        img: &Image,
        cell_width: u32,
        cell_height: u32,
    ) -> Result<RenderPlacement, PlacementError> {
        // In this function, there is a variable naming convention to try
        // to make it slightly less confusing. The prefix will tell you what
        // coordinate/size space a variable lives in:
        // - img_* is for the original image
        // - p_* is for the final placement
        // - vp_* is for the virtual placement

        // Determine the grid size that this virtual placement fits into.
        let p_grid = Self::grid(target, img, cell_width, cell_height)?;

        // From here on out we switch to floating point math. These are
        // constants that we'll reference repeatedly.
        let img_width_f64: f64 = img.width as f64;
        let img_height_f64: f64 = img.height as f64;

        // Next we have to fit the source image into the grid size while preserving
        // aspect ratio. We will center the image horizontally/vertically if
        // necessary.
        //
        // The offsets are pixels from the top-left of the placement-sized
        // image in order to center the image as necessary. The multipliers
        // are applied to the width/height of the original image size in
        // order to reach the placement size.
        struct PScale {
            x_offset: f64,
            y_offset: f64,
            x_scale: f64,
            y_scale: f64,
        }
        let p_scale: PScale = {
            let p_rows_px: f64 = (p_grid.rows * cell_height) as f64;
            let p_cols_px: f64 = (p_grid.columns * cell_width) as f64;
            if img_width_f64 * p_rows_px > img_height_f64 * p_cols_px {
                // Image is wider than the grid, fit width and center height
                let x_scale = p_cols_px / img_width_f64.max(1.0);
                let y_scale = x_scale;
                let y_offset = (p_rows_px - img_height_f64 * y_scale) / 2.0;
                PScale {
                    x_offset: 0.0,
                    y_offset,
                    x_scale,
                    y_scale,
                }
            } else {
                // Image is taller than the grid, fit height and center width
                let y_scale = p_rows_px / img_height_f64.max(1.0);
                let x_scale = y_scale;
                let x_offset = (p_cols_px - img_width_f64 * x_scale) / 2.0;
                PScale {
                    x_offset,
                    y_offset: 0.0,
                    x_scale,
                    y_scale,
                }
            }
        };

        // Scale our original image according to the aspect ratio
        // and padding calculated for p_scale.
        struct ImgScaled {
            x_offset: f64,
            y_offset: f64,
            width: f64,
            height: f64,
        }
        let img_scaled: ImgScaled = {
            let x_offset: f64 = p_scale.x_offset / p_scale.x_scale;
            let y_offset: f64 = p_scale.y_offset / p_scale.y_scale;
            let width: f64 = img_width_f64 + (x_offset * 2.0);
            let height: f64 = img_height_f64 + (y_offset * 2.0);
            ImgScaled {
                x_offset,
                y_offset,
                width,
                height,
            }
        };

        // Calculate the source rectangle for the scaled image. These
        // coordinates are in the scaled image space.
        struct Rect {
            x: f64,
            y: f64,
            width: f64,
            height: f64,
        }
        let mut img_scale_source: Rect = {
            // Float-converted values we already have
            let vp_width: f64 = self.width as f64;
            let vp_height: f64 = self.height as f64;
            let vp_col: f64 = self.col as f64;
            let vp_row: f64 = self.row as f64;
            let p_grid_cols: f64 = p_grid.columns as f64;
            let p_grid_rows: f64 = p_grid.rows as f64;

            // Calculate the scaled source rectangle for the image, undoing
            // the aspect ratio scaling as necessary.
            let width: f64 = img_scaled.width * (vp_width / p_grid_cols);
            let height: f64 = img_scaled.height * (vp_height / p_grid_rows);
            let x: f64 = img_scaled.width * (vp_col / p_grid_cols);
            let y: f64 = img_scaled.height * (vp_row / p_grid_rows);

            Rect {
                x,
                y,
                width,
                height,
            }
        };

        // The destination rectangle. The x/y is specified by offsets from
        // the top-left since that's how our RenderPlacement works.
        let p_dest: Rect = {
            let mut x_offset: f64 = 0.0;
            let mut y_offset: f64 = 0.0;
            let mut width: f64 = (self.width * cell_width) as f64;
            let mut height: f64 = (self.height * cell_height) as f64;

            if img_scale_source.y < img_scaled.y_offset {
                // If our source rect y is within the offset area, we need to
                // adjust our source rect and destination since the source texture
                // doesn't actually have the offset area blank.
                let offset: f64 = img_scaled.y_offset - img_scale_source.y;
                img_scale_source.height -= offset;
                y_offset = offset;
                height -= offset * p_scale.y_scale;
                img_scale_source.y = 0.0;

                // If our height is greater than our original height,
                // bring it back down. This addresses the case where the top
                // and bottom offsets are both used.
                if img_scale_source.height > img_height_f64 {
                    img_scale_source.height = img_height_f64;
                    height = img_height_f64 * p_scale.y_scale;
                }
            } else if img_scale_source.y + img_scale_source.height
                > img_scaled.height - img_scaled.y_offset
            {
                // if our y is in our bottom offset area, we need to shorten the
                // source to fit in the cell.
                img_scale_source.y -= img_scaled.y_offset;
                img_scale_source.height =
                    img_scaled.height - img_scaled.y_offset - img_scale_source.y;
                img_scale_source.height -= img_scaled.y_offset;
                height = img_scale_source.height * p_scale.y_scale;
            } else {
                img_scale_source.y -= img_scaled.y_offset;
            }

            if img_scale_source.x < img_scaled.x_offset {
                // If our source rect x is within the offset area, we need to
                // adjust our source rect and destination since the source texture
                // doesn't actually have the offset area blank.
                let offset: f64 = img_scaled.x_offset - img_scale_source.x;
                img_scale_source.width -= offset;
                x_offset = offset;
                width -= offset * p_scale.x_scale;
                img_scale_source.x = 0.0;

                // If our width is greater than our original width,
                // bring it back down. This addresses the case where the left
                // and right offsets are both used.
                if img_scale_source.width > img_width_f64 {
                    img_scale_source.width = img_width_f64;
                    width = img_width_f64 * p_scale.x_scale;
                }
            } else if img_scale_source.x + img_scale_source.width
                > img_scaled.width - img_scaled.x_offset
            {
                // if our x is in our right offset area, we need to shorten the
                // source to fit in the cell.
                img_scale_source.x -= img_scaled.x_offset;
                img_scale_source.width =
                    img_scaled.width - img_scaled.x_offset - img_scale_source.x;
                img_scale_source.width -= img_scaled.x_offset;
                width = img_scale_source.width * p_scale.x_scale;
            } else {
                img_scale_source.x -= img_scaled.x_offset;
            }

            // If our modified source width/height is less than zero then
            // we render nothing because it means we're rendering outside
            // of the visible image.
            if img_scale_source.width <= 0.0 || img_scale_source.height <= 0.0 {
                return Ok(RenderPlacement::new(self.pin));
            }

            Rect {
                x: x_offset * p_scale.x_scale,
                y: y_offset * p_scale.y_scale,
                width,
                height,
            }
        };

        Ok(RenderPlacement {
            top_left: self.pin,
            offset_x: p_dest.x.round() as u32,
            offset_y: p_dest.y.round() as u32,
            source_x: img_scale_source.x.round() as u32,
            source_y: img_scale_source.y.round() as u32,
            source_width: img_scale_source.width.round() as u32,
            source_height: img_scale_source.height.round() as u32,
            dest_width: p_dest.width.round() as u32,
            dest_height: p_dest.height.round() as u32,
        })
    }

    // Calculate the grid size for the placement. For virtual placements,
    // we use the requested row/cols. If either isn't specified, we choose
    // the best size based on the image size to fit the entire image in its
    // original size.
    //
    // This part of the code does NOT do preserve any aspect ratios. Its
    // dumbly fitting the image into the grid size -- possibly user specified.
    fn grid(
        placement: &StoragePlacement,
        image: &Image,
        cell_width: u32,
        cell_height: u32,
    ) -> Result<Grid, PlacementError> {
        // Use requested rows/columns if specified
        // For unspecified rows/columns, calculate based on the image size.
        let mut rows = placement.rows;
        let mut columns = placement.columns;
        if rows == 0 {
            rows = (image.height + cell_height - 1) / cell_height;
        }
        if columns == 0 {
            columns = (image.width + cell_width - 1) / cell_width;
        }
        Ok(Grid {
            rows: CellCountInt::try_from(rows)
                .map_err(|_| PlacementError::PlacementGridOutOfBounds)? as u32,
            columns: CellCountInt::try_from(columns)
                .map_err(|_| PlacementError::PlacementGridOutOfBounds)? as u32,
        })
    }
}

/// IncompletePlacement is the placement information present in a single
/// cell. It is "incomplete" because the specification allows for missing
/// diacritics and so on that continue from previous valid placements.
///
/// The render state (see `terminal.RenderState`) uses this to group
/// placeholder cells into runs while it rebuilds rows. It doesn't store a
/// position. The caller keeps track of where each run starts.
///
/// Building a run looks like this:
///
/// ```text
/// let mut run = IncompletePlacement::init(&style, graphemes).start();
/// // For each following placeholder cell:
/// let next = IncompletePlacement::init(&next_style, next_graphemes);
/// if !run.append(&next) {
///     // `run` is finished. `next` starts a new run, which
///     // continues from `next.start()`.
/// }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IncompletePlacement {
    /// Lower 24 bits of the image ID. This is specified in the fg color
    /// and is always required. (Zig: `u24`.)
    pub image_id_low: u32,

    /// Higher 8 bits of the image ID specified using the 3rd diacritic.
    /// This is optional.
    pub image_id_high: Option<u8>,

    /// Placement ID is optionally specified in the underline color.
    /// (Zig: `?u24`.)
    pub placement_id: Option<u32>,

    /// The row/col index for the image. These are 0-indexed. These
    /// are specified using diacritics. The row is first and the col
    /// is second. Both are optional. If not specified, they can continue
    /// a previous placement under certain conditions.
    pub row: Option<u32>,
    pub col: Option<u32>,

    /// The run width so far in cells.
    pub width: u32,
}

impl IncompletePlacement {
    /// Read the placement information from one placeholder cell, given
    /// its style and its combining characters (an empty slice if it has
    /// none).
    pub fn init(style: &Style, cps: &[u32]) -> IncompletePlacement {
        let mut result = IncompletePlacement {
            image_id_low: Self::color_to_id(style.fg_color),
            image_id_high: None,
            placement_id: {
                let id = Self::color_to_id(style.underline_color);
                if id != 0 {
                    Some(id)
                } else {
                    None
                }
            },
            row: None,
            col: None,
            width: 1,
        };

        // Try to decode all our diacritics. Any invalid diacritics are
        // treated as if they don't exist. This isn't explicitly specified
        // at the time of writing this but it appears to be how Kitty behaves.
        if !cps.is_empty() {
            result.row = get_index(cps[0]).or_else(|| {
                log::warn!(target: LOG_TARGET, "virtual placement with invalid row diacritic cp={:X}", cps[0]);
                None
            });

            if cps.len() > 1 {
                result.col = get_index(cps[1]).or_else(|| {
                    log::warn!(target: LOG_TARGET, "virtual placement with invalid col diacritic cp={:X}", cps[1]);
                    None
                });

                if cps.len() > 2 {
                    let high_ = get_index(cps[2]).or_else(|| {
                        log::warn!(target: LOG_TARGET, "virtual placement with invalid high diacritic cp={:X}", cps[2]);
                        None
                    });

                    if let Some(high) = high_ {
                        result.image_id_high = u8::try_from(high).ok().or_else(|| {
                            log::warn!(
                                target: LOG_TARGET,
                                "virtual placement with invalid high diacritic cp={:X} value={}",
                                cps[2],
                                high
                            );
                            None
                        });
                    }

                    // Any additional diacritics are ignored.
                }
            }
        }

        result
    }

    /// Returns this cell as the first cell of a new run. A cell that
    /// leaves out its row or column would normally continue the run to
    /// its left. At the start of a run there is nothing to continue, so
    /// a missing row or column is zero.
    pub fn start(self) -> IncompletePlacement {
        let mut result = self;
        if result.row.is_none() {
            result.row = Some(0);
        }
        if result.col.is_none() {
            result.col = Some(0);
        }
        result
    }

    /// Append this incomplete placement to an existing placement to
    /// create a run. This returns true if the placements are compatible
    /// and were combined. If this returns false, the other placement is
    /// unchanged.
    pub fn append(&mut self, other: &IncompletePlacement) -> bool {
        if !self.can_append(other) {
            return false;
        }
        self.width += 1;
        true
    }

    fn can_append(&self, other: &IncompletePlacement) -> bool {
        // Converted from Kitty's logic, don't @ me.
        self.image_id_low == other.image_id_low
            && self.placement_id == other.placement_id
            && (other.row.is_none() || other.row == self.row)
            && (other.col.is_none() || other.col == Some(self.col.unwrap() + self.width))
            && (other.image_id_high.is_none() || other.image_id_high == self.image_id_high)
    }

    /// The full image ID. The low 24 bits come from the foreground color
    /// and the high 8 bits from the optional third diacritic.
    pub fn image_id(&self) -> u32 {
        let low: u32 = self.image_id_low;
        let high: u32 = self.image_id_high.unwrap_or(0) as u32;
        low | (high << 24)
    }

    /// Convert a style color to a Kitty image protocol ID. This works by
    /// taking the 24 most significant bits of the color, which lets it work
    /// for both palette and rgb-based colors.
    fn color_to_id(c: Color) -> u32 {
        match c {
            Color::None => 0,
            Color::Palette(v) => v as u32,
            Color::Rgb(rgb) => {
                let r: u32 = rgb.r as u32;
                let g: u32 = rgb.g as u32;
                let b: u32 = rgb.b as u32;
                (r << 16) | (g << 8) | b
            }
        }
    }
}

/// Get the row/col index for a diacritic codepoint. These are 0-indexed.
///
/// Rebuilding a row of placeholders calls this up to three times per
/// cell, so it needs to be fast. Most diacritics are below U+2E00 and
/// are found with a single table lookup. The few above that use a
/// binary search.
fn get_index(cp: u32) -> Option<u32> {
    if cp < DENSE_END {
        if cp < DENSE_START {
            return None;
        }
        let idx = DENSE_INDEX[(cp - DENSE_START) as usize];
        return if idx == DENSE_NONE { None } else { Some(idx as u32) };
    }

    let sparse = &DIACRITICS[SPARSE_START..];
    let idx = sparse.binary_search(&cp).ok()?;
    Some((SPARSE_START + idx) as u32)
}

/// The range of codepoints covered by `dense_index`. It holds every
/// diacritic below U+2E00 (all but a few dozen of them) in about 11 KB,
/// and each of their indexes fits in a byte.
const DENSE_START: u32 = DIACRITICS[0];
const DENSE_END: u32 = 0x2E00;
const DENSE_NONE: u8 = u8::MAX;

/// The index in `diacritics` of the first codepoint at or above
/// `dense_end`. Codepoints from here on are found with a binary search.
const SPARSE_START: usize = {
    let mut i = 0;
    while i < DIACRITICS.len() {
        if DIACRITICS[i] >= DENSE_END {
            break;
        }
        i += 1;
    }
    i
};

/// Maps `cp - dense_start` to the index of `cp` in `diacritics`, or to
/// `dense_none` if `cp` is not a diacritic.
static DENSE_INDEX: [u8; (DENSE_END - DENSE_START) as usize] = {
    assert!(SPARSE_START < DENSE_NONE as usize, "dense diacritic index overflows u8");
    let mut table = [DENSE_NONE; (DENSE_END - DENSE_START) as usize];
    let mut i = 0;
    while i < SPARSE_START {
        table[(DIACRITICS[i] - DENSE_START) as usize] = i as u8;
        i += 1;
    }
    table
};

/// These are the diacritics used with the Kitty graphics protocol
/// Unicode placement feature to specify the row/column for placement.
/// The index into the array determines the value.
///
/// This is derived from:
/// https://sw.kovidgoyal.net/kitty/_downloads/f0a0de9ec8d9ff4456206db8e0814937/rowcolumn-diacritics.txt
const DIACRITICS: &[u32] = &[
    0x0305, 0x030D, 0x030E, 0x0310, 0x0312, 0x033D, 0x033E, 0x033F, 0x0346, 0x034A,
    0x034B, 0x034C, 0x0350, 0x0351, 0x0352, 0x0357, 0x035B, 0x0363, 0x0364, 0x0365,
    0x0366, 0x0367, 0x0368, 0x0369, 0x036A, 0x036B, 0x036C, 0x036D, 0x036E, 0x036F,
    0x0483, 0x0484, 0x0485, 0x0486, 0x0487, 0x0592, 0x0593, 0x0594, 0x0595, 0x0597,
    0x0598, 0x0599, 0x059C, 0x059D, 0x059E, 0x059F, 0x05A0, 0x05A1, 0x05A8, 0x05A9,
    0x05AB, 0x05AC, 0x05AF, 0x05C4, 0x0610, 0x0611, 0x0612, 0x0613, 0x0614, 0x0615,
    0x0616, 0x0617, 0x0657, 0x0658, 0x0659, 0x065A, 0x065B, 0x065D, 0x065E, 0x06D6,
    0x06D7, 0x06D8, 0x06D9, 0x06DA, 0x06DB, 0x06DC, 0x06DF, 0x06E0, 0x06E1, 0x06E2,
    0x06E4, 0x06E7, 0x06E8, 0x06EB, 0x06EC, 0x0730, 0x0732, 0x0733, 0x0735, 0x0736,
    0x073A, 0x073D, 0x073F, 0x0740, 0x0741, 0x0743, 0x0745, 0x0747, 0x0749, 0x074A,
    0x07EB, 0x07EC, 0x07ED, 0x07EE, 0x07EF, 0x07F0, 0x07F1, 0x07F3, 0x0816, 0x0817,
    0x0818, 0x0819, 0x081B, 0x081C, 0x081D, 0x081E, 0x081F, 0x0820, 0x0821, 0x0822,
    0x0823, 0x0825, 0x0826, 0x0827, 0x0829, 0x082A, 0x082B, 0x082C, 0x082D, 0x0951,
    0x0953, 0x0954, 0x0F82, 0x0F83, 0x0F86, 0x0F87, 0x135D, 0x135E, 0x135F, 0x17DD,
    0x193A, 0x1A17, 0x1A75, 0x1A76, 0x1A77, 0x1A78, 0x1A79, 0x1A7A, 0x1A7B, 0x1A7C,
    0x1B6B, 0x1B6D, 0x1B6E, 0x1B6F, 0x1B70, 0x1B71, 0x1B72, 0x1B73, 0x1CD0, 0x1CD1,
    0x1CD2, 0x1CDA, 0x1CDB, 0x1CE0, 0x1DC0, 0x1DC1, 0x1DC3, 0x1DC4, 0x1DC5, 0x1DC6,
    0x1DC7, 0x1DC8, 0x1DC9, 0x1DCB, 0x1DCC, 0x1DD1, 0x1DD2, 0x1DD3, 0x1DD4, 0x1DD5,
    0x1DD6, 0x1DD7, 0x1DD8, 0x1DD9, 0x1DDA, 0x1DDB, 0x1DDC, 0x1DDD, 0x1DDE, 0x1DDF,
    0x1DE0, 0x1DE1, 0x1DE2, 0x1DE3, 0x1DE4, 0x1DE5, 0x1DE6, 0x1DFE, 0x20D0, 0x20D1,
    0x20D4, 0x20D5, 0x20D6, 0x20D7, 0x20DB, 0x20DC, 0x20E1, 0x20E7, 0x20E9, 0x20F0,
    0x2CEF, 0x2CF0, 0x2CF1, 0x2DE0, 0x2DE1, 0x2DE2, 0x2DE3, 0x2DE4, 0x2DE5, 0x2DE6,
    0x2DE7, 0x2DE8, 0x2DE9, 0x2DEA, 0x2DEB, 0x2DEC, 0x2DED, 0x2DEE, 0x2DEF, 0x2DF0,
    0x2DF1, 0x2DF2, 0x2DF3, 0x2DF4, 0x2DF5, 0x2DF6, 0x2DF7, 0x2DF8, 0x2DF9, 0x2DFA,
    0x2DFB, 0x2DFC, 0x2DFD, 0x2DFE, 0x2DFF, 0xA66F, 0xA67C, 0xA67D, 0xA6F0, 0xA6F1,
    0xA8E0, 0xA8E1, 0xA8E2, 0xA8E3, 0xA8E4, 0xA8E5, 0xA8E6, 0xA8E7, 0xA8E8, 0xA8E9,
    0xA8EA, 0xA8EB, 0xA8EC, 0xA8ED, 0xA8EE, 0xA8EF, 0xA8F0, 0xA8F1, 0xAAB0, 0xAAB2,
    0xAAB3, 0xAAB7, 0xAAB8, 0xAABE, 0xAABF, 0xAAC1, 0xFE20, 0xFE21, 0xFE22, 0xFE23,
    0xFE24, 0xFE25, 0xFE26, 0x10A0F, 0x10A38, 0x1D185, 0x1D186, 0x1D187, 0x1D188, 0x1D189,
    0x1D1AA, 0x1D1AB, 0x1D1AC, 0x1D1AD, 0x1D242, 0x1D243, 0x1D244,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_diacritic_sorted() {
        // diacritics must be sorted since we use a binary search.
        assert!(DIACRITICS.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn unicode_diacritic() {
        // Some spot checks based on Kitty behavior
        assert_eq!(30, get_index(0x483).unwrap());
        assert_eq!(294, get_index(0x1d242).unwrap());
    }

    #[test]
    fn unicode_diacritic_lookup_matches_table() {
        // Every diacritic maps to its index, through both the dense table
        // and the binary search.
        for (i, &cp) in DIACRITICS.iter().enumerate() {
            assert_eq!(Some(i as u32), get_index(cp));
        }

        // Nothing else maps, including around both ranges.
        let mut cp: u32 = 0;
        while cp < 0x1E000 {
            if !DIACRITICS.contains(&cp) {
                assert_eq!(None, get_index(cp));
            }
            cp += 1;
        }
    }

    #[test]
    fn incomplete_placement_run() {
        let style = Style {
            fg_color: Color::Palette(1),
            ..Default::default()
        };
        let mut run = IncompletePlacement::init(&style, &[DIACRITICS[0], DIACRITICS[0]]).start();
        assert_eq!(run.row, Some(0));
        assert_eq!(run.col, Some(0));
        assert_eq!(run.image_id(), 1);
        let next = IncompletePlacement::init(&style, &[DIACRITICS[0], DIACRITICS[1]]);
        assert!(run.append(&next));
        assert_eq!(run.width, 2);
        let bad = IncompletePlacement::init(&style, &[DIACRITICS[1]]);
        assert!(!run.append(&bad));
    }

    use crate::terminal::kitty::graphics_storage::test_util::init;
    use crate::terminal::kitty::graphics_storage::{ImageStorage, Location};

    /// Shared setup of the "unicode render placement" tests: a 100x100
    /// terminal with a 500x306 image (id 1) and a virtual placement of
    /// `columns`x`rows` cells. Returns the render placement of row `row`
    /// of a 4-cell-wide run at the cursor.
    fn dog_render_placement(columns: u32, rows: u32, row: u32) -> RenderPlacement {
        let cell_width = 36;
        let cell_height = 80;

        let mut t = init(100, 100);
        let mut s = ImageStorage::default();

        let image = Image {
            id: 1,
            width: 500,
            height: 306,
            ..Default::default()
        };
        s.add_image(&mut t.screens.active.pages, image.clone())
            .unwrap();
        s.add_placement(
            &mut t.screens.active.pages,
            1,
            0,
            StoragePlacement {
                columns,
                rows,
                ..StoragePlacement::new(Location::Virtual)
            },
        )
        .unwrap();

        // SAFETY: the cursor pin is tracked by the screen.
        let pin = unsafe { *t.screens.active.cursor.page_pin.as_ptr() };
        let p = Placement {
            pin,
            image_id: 1,
            placement_id: 0,
            col: 0,
            row,
            width: 4,
            height: 1,
        };
        let rp = p
            .render_placement(
                &s.placeholder_target(1, 0).unwrap().placement,
                &image,
                cell_width,
                cell_height,
            )
            .unwrap();
        s.deinit(&mut t.screens.active.pages);
        rp
    }

    #[test]
    fn unicode_render_placement_dog_4x2() {
        // Row 1
        {
            let rp = dog_render_placement(4, 2, 0);
            assert_eq!(0, rp.offset_x);
            assert_eq!(36, rp.offset_y);
            assert_eq!(0, rp.source_x);
            assert_eq!(0, rp.source_y);
            assert_eq!(500, rp.source_width);
            assert_eq!(153, rp.source_height);
            assert_eq!(144, rp.dest_width);
            assert_eq!(44, rp.dest_height);
        }
        // Row 2
        {
            let rp = dog_render_placement(4, 2, 1);
            assert_eq!(0, rp.offset_x);
            assert_eq!(0, rp.offset_y);
            assert_eq!(0, rp.source_x);
            assert_eq!(153, rp.source_y);
            assert_eq!(500, rp.source_width);
            assert_eq!(153, rp.source_height);
            assert_eq!(144, rp.dest_width);
            assert_eq!(44, rp.dest_height);
        }
    }

    // Fish:
    // printf "\033_Gf=100,i=1,t=f,q=2;$(printf dog.png | base64)\033\\"
    // printf "\e[38;5;1m\U10EEEE\U0305\U0305\U10EEEE\U0305\U030D\U10EEEE\U0305\U030E\U10EEEE\U0305\U0310\e[39m\n"
    // printf "\e[38;5;1m\U10EEEE\U030D\U0305\U10EEEE\U030D\U030D\U10EEEE\U030D\U030E\U10EEEE\U030D\U0310\e[39m\n"
    // printf "\033_Ga=p,i=1,U=1,q=2,c=2,r=2\033\\"
    #[test]
    fn unicode_render_placement_dog_2x2_with_blank_cells() {
        // Row 1
        {
            let rp = dog_render_placement(2, 2, 0);
            assert_eq!(0, rp.offset_x);
            assert_eq!(58, rp.offset_y);
            assert_eq!(0, rp.source_x);
            assert_eq!(0, rp.source_y);
            assert_eq!(500, rp.source_width);
            assert_eq!(153, rp.source_height);
            assert_eq!(72, rp.dest_width);
            assert_eq!(22, rp.dest_height);
        }
        // Row 2
        {
            let rp = dog_render_placement(2, 2, 1);
            assert_eq!(0, rp.offset_x);
            assert_eq!(0, rp.offset_y);
            assert_eq!(0, rp.source_x);
            assert_eq!(153, rp.source_y);
            assert_eq!(500, rp.source_width);
            assert_eq!(153, rp.source_height);
            assert_eq!(72, rp.dest_width);
            assert_eq!(22, rp.dest_height);
        }
    }

    // Fish:
    // printf "\033_Gf=100,i=1,t=f,q=2;$(printf dog.png | base64)\033\\"
    // printf "\e[38;5;1m\U10EEEE\U0305\U0305\U10EEEE\U0305\U030D\U10EEEE\U0305\U030E\U10EEEE\U0305\U0310\e[39m\n"
    // printf "\033_Ga=p,i=1,U=1,q=2,c=1,r=1\033\\"
    #[test]
    fn unicode_render_placement_dog_1x1() {
        // Row 1
        let rp = dog_render_placement(1, 1, 0);
        assert_eq!(0, rp.offset_x);
        assert_eq!(29, rp.offset_y);
        assert_eq!(0, rp.source_x);
        assert_eq!(0, rp.source_y);
        assert_eq!(500, rp.source_width);
        assert_eq!(306, rp.source_height);
        assert_eq!(36, rp.dest_width);
        assert_eq!(22, rp.dest_height);
    }
}
