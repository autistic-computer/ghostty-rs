//! Port of `src/font/Glyph.zig`.
//!
//! STATUS: ported
//!
//! Glyph is a single loaded glyph for a face.

use super::metrics::Metrics;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Glyph {
    /// width of glyph in pixels
    pub width: u32,

    /// height of glyph in pixels
    pub height: u32,

    /// left bearing
    pub offset_x: i32,

    /// top bearing
    pub offset_y: i32,

    /// coordinates in the atlas of the top-left corner. These have to
    /// be normalized to be between 0 and 1 prior to use in shaders.
    pub atlas_x: u32,
    pub atlas_y: u32,
}

/// The size and position of a glyph.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Size {
    pub width: f64,
    pub height: f64,
    pub x: f64,
    pub y: f64,
}

/// Metrics describing the authored glyph coordinate space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DesignMetrics {
    /// Units-per-em for outline/design coordinates.
    pub units_per_em: u32,

    /// Authored advance width in design units.
    pub advance_width: u32,

    /// Authored line height in design units.
    pub line_height: u32,
}

/// Additional options for rendering glyphs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderOptions {
    /// The metrics that are defining the grid layout. These are usually
    /// the metrics of the primary font face. The grid metrics are used
    /// by the font face to better layout the glyph in situations where
    /// the font is not exactly the same size as the grid.
    pub grid_metrics: Metrics,

    /// The number of grid cells this glyph will take up. This can be used
    /// optionally by the rasterizer to better layout the glyph.
    /// (Zig `?u2`, default `null`.)
    pub cell_width: Option<u8>,

    /// Constraint and alignment properties for the glyph. The rasterizer
    /// should call the `constrain` function on this with the original size
    /// and bearings of the glyph to get remapped values that the glyph
    /// should be scaled/moved to.
    pub constraint: Constraint,

    /// The number of cells, horizontally that the glyph is free to take up
    /// when resized and aligned by `constraint`. This is usually 1, but if
    /// there's whitespace to the right of the cell then it can be 2.
    /// (Zig `u2`, default `1`.)
    pub constraint_width: u8,

    /// Thicken the glyph. This draws the glyph with a thicker stroke width.
    /// This is purely an aesthetic setting.
    ///
    /// This only works with CoreText currently.
    pub thicken: bool,

    /// "Strength" of the thickening, between `0` and `255`.
    /// Only has an effect when `thicken` is enabled.
    ///
    /// `0` does not correspond to *no* thickening,
    /// just the *lightest* thickening available.
    ///
    /// CoreText only.
    pub thicken_strength: u8,
}

impl RenderOptions {
    /// Render options with the given grid metrics and every other field
    /// set to its default value (Zig `.{ .grid_metrics = m }`).
    pub fn new(grid_metrics: Metrics) -> RenderOptions {
        RenderOptions {
            grid_metrics,
            cell_width: None,
            constraint: Constraint::NONE,
            constraint_width: 1,
            thicken: false,
            thicken_strength: 255,
        }
    }
}

/// See the `constraint` field of [`RenderOptions`]
/// (Zig `RenderOptions.Constraint`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Constraint {
    /// Sizing rule.
    pub size: ConstraintSize,

    /// Vertical alignment rule.
    pub align_vertical: ConstraintAlign,
    /// Horizontal alignment rule.
    pub align_horizontal: ConstraintAlign,

    /// Top padding when resizing.
    pub pad_top: f64,
    /// Left padding when resizing.
    pub pad_left: f64,
    /// Right padding when resizing.
    pub pad_right: f64,
    /// Bottom padding when resizing.
    pub pad_bottom: f64,

    // Size and bearings of the glyph relative
    // to the bounding box of its scale group.
    pub relative_width: f64,
    pub relative_height: f64,
    pub relative_x: f64,
    pub relative_y: f64,

    /// Maximum aspect ratio (width/height) to allow when stretching.
    pub max_xy_ratio: Option<f64>,

    /// Maximum number of cells horizontally to use. (Zig `u2`.)
    pub max_constraint_width: u8,

    /// What to use as the height metric when constraining the glyph and
    /// the constraint width is 1,
    pub height: ConstraintHeight,
}

impl Default for Constraint {
    fn default() -> Self {
        Constraint::NONE
    }
}

/// Zig `RenderOptions.Constraint.Size`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConstraintSize {
    /// Don't change the size of this glyph.
    None,
    /// Scale the glyph down if needed to fit within the bounds,
    /// preserving aspect ratio.
    Fit,
    /// Scale the glyph up or down to exactly match the bounds,
    /// preserving aspect ratio.
    Cover,
    /// Scale the glyph down if needed to fit within the bounds,
    /// preserving aspect ratio. If the glyph doesn't cover a
    /// single cell, scale up. If the glyph exceeds a single
    /// cell but is within the bounds, do nothing.
    /// (Nerd Font specific rule.)
    FitCover1,
    /// Stretch the glyph to exactly fit the bounds in both
    /// directions, disregarding aspect ratio.
    Stretch,
}

/// Zig `RenderOptions.Constraint.Align`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConstraintAlign {
    /// Don't move the glyph on this axis.
    None,
    /// Move the glyph so that its leading (bottom/left)
    /// edge aligns with the leading edge of the axis.
    Start,
    /// Move the glyph so that its trailing (top/right)
    /// edge aligns with the trailing edge of the axis.
    End,
    /// Move the glyph so that it is centered on this axis.
    Center,
    /// Move the glyph so that it is centered on this axis,
    /// but always with respect to the first cell even for
    /// multi-cell constraints. (Nerd Font specific rule.)
    Center1,
}

/// Zig `RenderOptions.Constraint.Height`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConstraintHeight {
    /// Use the full line height of the primary face for
    /// constraining this glyph.
    Cell,
    /// Use the icon height from the grid metrics for
    /// constraining this glyph. Unlike `cell`, the value of
    /// this height depends on both the constraint width and the
    /// affected by the `adjust-icon-height` config option.
    Icon,
}

impl Constraint {
    /// Don't constrain the glyph in any way.
    pub const NONE: Constraint = Constraint {
        size: ConstraintSize::None,
        align_vertical: ConstraintAlign::None,
        align_horizontal: ConstraintAlign::None,
        pad_top: 0.0,
        pad_left: 0.0,
        pad_right: 0.0,
        pad_bottom: 0.0,
        relative_width: 1.0,
        relative_height: 1.0,
        relative_x: 0.0,
        relative_y: 0.0,
        max_xy_ratio: None,
        max_constraint_width: 2,
        height: ConstraintHeight::Cell,
    };

    /// Returns true if the constraint does anything. If it doesn't,
    /// because it neither sizes nor positions the glyph, then this
    /// returns false.
    #[inline]
    pub fn does_anything(&self) -> bool {
        self.size != ConstraintSize::None
            || self.align_horizontal != ConstraintAlign::None
            || self.align_vertical != ConstraintAlign::None
    }

    /// Apply this constraint to the provided glyph
    /// size, given the available width and height.
    ///
    /// `constraint_width` is the number of cells horizontally available
    /// for this glyph.
    pub fn constrain(&self, glyph: Size, metrics: Metrics, constraint_width: u8) -> Size {
        if !self.does_anything() {
            return glyph;
        }

        match self.size {
            ConstraintSize::Stretch => {
                // Stretched glyphs are usually meant to align across cell
                // boundaries, which works best if they're scaled and
                // aligned to the grid rather than the face. This is most
                // easily done by inserting this little fib in the metrics.
                let mut m = metrics;
                m.face_width = m.cell_width as f64;
                m.face_height = m.cell_height as f64;
                m.face_y = 0.0;

                // Negative padding for stretched glyphs is a band-aid to
                // avoid gaps due to pixel rounding, but at the cost of
                // unsightly overlap artifacts. Since we scale and align to
                // the grid rather than the face, we don't need it.
                let mut c = *self;
                c.pad_bottom = c.pad_bottom.max(0.0);
                c.pad_top = c.pad_top.max(0.0);
                c.pad_left = c.pad_left.max(0.0);
                c.pad_right = c.pad_right.max(0.0);

                c.constrain_inner(glyph, m, constraint_width)
            }
            _ => self.constrain_inner(glyph, metrics, constraint_width),
        }
    }

    fn constrain_inner(&self, glyph: Size, metrics: Metrics, constraint_width: u8) -> Size {
        // For extra wide font faces, never stretch glyphs across two cells.
        // This mirrors font_patcher.
        let min_constraint_width: u8 = if self.size == ConstraintSize::Stretch
            && metrics.face_width > 0.9 * metrics.face_height
        {
            1
        } else {
            self.max_constraint_width.min(constraint_width)
        };

        // The bounding box for the glyph's scale group.
        // Scaling and alignment rules are calculated for
        // this box and then applied to the glyph.
        let mut group: Size = {
            let group_width = glyph.width / self.relative_width;
            let group_height = glyph.height / self.relative_height;
            Size {
                width: group_width,
                height: group_height,
                x: glyph.x - (group_width * self.relative_x),
                y: glyph.y - (group_height * self.relative_y),
            }
        };

        // Apply prescribed scaling, preserving the
        // center bearings of the group bounding box
        let (width_factor, height_factor) = self.scale_factors(group, metrics, min_constraint_width);
        let center_x = group.x + (group.width / 2.0);
        let center_y = group.y + (group.height / 2.0);
        group.width *= width_factor;
        group.height *= height_factor;
        group.x = center_x - (group.width / 2.0);
        group.y = center_y - (group.height / 2.0);

        // NOTE: font_patcher jumps through a lot of hoops at this
        // point to ensure that the glyph remains within the target
        // bounding box after rounding to font definition units.
        // This is irrelevant here as we're not rounding, we're
        // staying in f64 and heading straight to rendering.

        // Apply prescribed alignment
        group.y = self.aligned_y(group, metrics);
        group.x = self.aligned_x(group, metrics, min_constraint_width);

        // Transfer the scaling and alignment back to the glyph and return.
        Size {
            width: width_factor * glyph.width,
            height: height_factor * glyph.height,
            x: group.x + (group.width * self.relative_x),
            y: group.y + (group.height * self.relative_y),
        }
    }

    /// Return width and height scaling factors for this scaling group.
    fn scale_factors(&self, group: Size, metrics: Metrics, min_constraint_width: u8) -> (f64, f64) {
        if self.size == ConstraintSize::None {
            return (1.0, 1.0);
        }

        let multi_cell = min_constraint_width > 1;

        let pad_width_factor = min_constraint_width as f64 - (self.pad_left + self.pad_right);
        let pad_height_factor = 1.0 - (self.pad_bottom + self.pad_top);

        let target_width = pad_width_factor * metrics.face_width;
        let target_height = pad_height_factor
            * match self.height {
                ConstraintHeight::Cell => metrics.face_height,
                // Like font-patcher, the icon constraint height depends on the
                // constraint width. Unlike font-patcher, the multi-cell
                // icon_height may be different from face_height due to the
                // `adjust-icon-height` config option.
                ConstraintHeight::Icon => {
                    if multi_cell {
                        metrics.icon_height
                    } else {
                        metrics.icon_height_single
                    }
                }
            };

        let mut width_factor = target_width / group.width;
        let mut height_factor = target_height / group.height;

        match self.size {
            ConstraintSize::None => unreachable!(),
            ConstraintSize::Fit => {
                // Scale down to fit if needed
                height_factor = zig_min(1.0, zig_min(width_factor, height_factor));
                width_factor = height_factor;
            }
            ConstraintSize::Cover => {
                // Scale to cover
                height_factor = zig_min(width_factor, height_factor);
                width_factor = height_factor;
            }
            ConstraintSize::FitCover1 => {
                // Scale down to fit or up to cover at least one cell
                // NOTE: This is similar to font_patcher's "pa" mode,
                // however, font_patcher will only do the upscaling
                // part if the constraint width is 1, resulting in
                // some icons becoming smaller when the constraint
                // width increases. You'd see icons shrinking when
                // opening up a space after them. This makes no
                // sense, so we've fixed the rule such that these
                // icons are scaled to the same size for multi-cell
                // constraints as they would be for single-cell.
                height_factor = zig_min(width_factor, height_factor);
                if multi_cell && height_factor > 1.0 {
                    // Call back into this function with
                    // constraint width 1 to get single-cell scale
                    // factors. We use the height factor as width
                    // could have been modified by max_xy_ratio.
                    let (_, single_height_factor) = self.scale_factors(group, metrics, 1);
                    height_factor = zig_max(1.0, single_height_factor);
                }
                width_factor = height_factor;
            }
            ConstraintSize::Stretch => {}
        }

        // Reduce aspect ratio if required
        if let Some(ratio) = self.max_xy_ratio {
            if group.width * width_factor > group.height * height_factor * ratio {
                width_factor = group.height * height_factor * ratio / group.width;
            }
        }

        (width_factor, height_factor)
    }

    /// Return vertical bearing for aligning this group
    fn aligned_y(&self, group: Size, metrics: Metrics) -> f64 {
        if self.size == ConstraintSize::None && self.align_vertical == ConstraintAlign::None {
            // If we don't have any constraints affecting the vertical axis,
            // we don't touch vertical alignment.
            return group.y;
        }
        // We use face_height and offset by face_y, rather than
        // using cell_height directly, to account for the asymmetry
        // of the pixel cell around the face (a consequence of
        // aligning the baseline with a pixel boundary rather than
        // vertically centering the face).
        let pad_bottom_dy = self.pad_bottom * metrics.face_height;
        let pad_top_dy = self.pad_top * metrics.face_height;
        let start_y = metrics.face_y + pad_bottom_dy;
        let end_y = metrics.face_y + (metrics.face_height - group.height - pad_top_dy);
        let center_y = (start_y + end_y) / 2.0;
        match self.align_vertical {
            // NOTE: Even if there is no prescribed alignment, we ensure
            // that the group doesn't protrude outside the padded cell,
            // since this is implied by every available size constraint. If
            // the group is too high we fall back to centering, though if we
            // hit the .none prong we always have self.size != .none, so
            // this should never happen.
            ConstraintAlign::None => {
                if end_y < start_y {
                    center_y
                } else {
                    zig_max(start_y, zig_min(group.y, end_y))
                }
            }
            ConstraintAlign::Start => start_y,
            ConstraintAlign::End => end_y,
            ConstraintAlign::Center | ConstraintAlign::Center1 => center_y,
        }
    }

    /// Return horizontal bearing for aligning this group
    fn aligned_x(&self, group: Size, metrics: Metrics, min_constraint_width: u8) -> f64 {
        if self.size == ConstraintSize::None && self.align_horizontal == ConstraintAlign::None {
            // If we don't have any constraints affecting the horizontal
            // axis, we don't touch horizontal alignment.
            return group.x;
        }
        // For multi-cell constraints, we align relative to the span
        // from the left edge of the first cell to the right edge of
        // the last face cell assuming it's left-aligned within the
        // rounded and adjusted pixel cell. Any horizontal offset to
        // center the face within the grid cell is the responsibility
        // of the backend-specific rendering code, and should be done
        // after applying constraints.
        let full_face_span = metrics.face_width
            + ((min_constraint_width as u32 - 1) * metrics.cell_width) as f64;
        let pad_left_dx = self.pad_left * metrics.face_width;
        let pad_right_dx = self.pad_right * metrics.face_width;
        let start_x = pad_left_dx;
        let end_x = full_face_span - group.width - pad_right_dx;
        match self.align_horizontal {
            // NOTE: Even if there is no prescribed alignment, we ensure
            // that the glyph doesn't protrude outside the padded cell,
            // since this is implied by every available size constraint. The
            // left-side bound has priority if the group is too wide, though
            // if we hit the .none prong we always have self.size != .none,
            // so this should never happen.
            ConstraintAlign::None => zig_max(start_x, zig_min(group.x, end_x)),
            ConstraintAlign::Start => start_x,
            ConstraintAlign::End => zig_max(start_x, end_x),
            ConstraintAlign::Center => zig_max(start_x, (start_x + end_x) / 2.0),
            // NOTE: .center1 implements the font_patcher rule of centering
            // in the first cell even for multi-cell constraints. Since glyphs
            // are not allowed to protrude to the left, this results in the
            // left-alignment like .start when the glyph is wider than a cell.
            ConstraintAlign::Center1 => {
                let end1_x = metrics.face_width - group.width - pad_right_dx;
                zig_max(start_x, (start_x + end1_x) / 2.0)
            }
        }
    }
}

/// Zig `@min` for floats.
#[inline]
fn zig_min(a: f64, b: f64) -> f64 {
    a.min(b)
}

/// Zig `@max` for floats.
#[inline]
fn zig_max(a: f64, b: f64) -> f64 {
    a.max(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::font::nerd_font_attributes::get_constraint;

    crate::impl_approx_equal_fields!(Size { width, height, x, y });

    fn expect_approx_equal(expected: Size, actual: Size) {
        crate::datastruct::comparison::expect_approx_equal(&expected, &actual).unwrap();
    }

    #[test]
    fn constraints() {
        type GlyphSize = Size;

        // Hardcoded data matches metrics from CoreText at size 12 and DPI 96.

        // Define grid metrics (matches font-family = JetBrains Mono)
        let metrics = Metrics {
            cell_width: 10,
            cell_height: 22,
            cell_baseline: 5,
            underline_position: 19,
            underline_thickness: 1,
            strikethrough_position: 12,
            strikethrough_thickness: 1,
            overline_position: 0,
            overline_thickness: 1,
            box_thickness: 1,
            cursor_thickness: 1,
            cursor_height: 22,
            icon_height: 21.12,
            icon_height_single: 44.48 / 3.0,
            face_width: 9.6,
            face_height: 21.12,
            face_y: 0.2,
        };

        // ASCII (no constraint).
        {
            let constraint = Constraint::NONE;

            // BBox of 'x' from JetBrains Mono.
            let glyph_x = GlyphSize {
                width: 6.784,
                height: 15.28,
                x: 1.408,
                y: 4.84,
            };

            // Any constraint width: do nothing.
            for constraint_width in [1, 2] {
                expect_approx_equal(glyph_x, constraint.constrain(glyph_x, metrics, constraint_width));
            }
        }

        // Symbol (same constraint as hardcoded in Renderer.addGlyph).
        {
            let constraint = Constraint {
                size: ConstraintSize::Fit,
                ..Constraint::NONE
            };

            // BBox of '■' (0x25A0 black square) from Iosevka.
            // NOTE: This glyph is designed to span two cells.
            let glyph_25a0 = GlyphSize {
                width: 10.272,
                height: 10.272,
                x: 2.864,
                y: 5.304,
            };

            // Constraint width 1: scale down and shift to fit a single cell.
            expect_approx_equal(
                GlyphSize {
                    width: metrics.face_width,
                    height: metrics.face_width,
                    x: 0.0,
                    y: 5.64,
                },
                constraint.constrain(glyph_25a0, metrics, 1),
            );

            // Constraint width 2: do nothing.
            expect_approx_equal(glyph_25a0, constraint.constrain(glyph_25a0, metrics, 2));
        }

        // Emoji (same constraint as hardcoded in SharedGrid.renderGlyph).
        {
            let constraint = Constraint {
                size: ConstraintSize::Cover,
                align_horizontal: ConstraintAlign::Center,
                align_vertical: ConstraintAlign::Center,
                pad_left: 0.025,
                pad_right: 0.025,
                ..Constraint::NONE
            };

            // BBox of '🥸' (0x1F978) from Apple Color Emoji.
            let glyph_1f978 = GlyphSize {
                width: 20.0,
                height: 20.0,
                x: 0.46,
                y: 1.0,
            };

            // Constraint width 2: scale to cover two cells with padding, center;
            expect_approx_equal(
                GlyphSize {
                    width: 18.72,
                    height: 18.72,
                    x: 0.44,
                    y: 1.4,
                },
                constraint.constrain(glyph_1f978, metrics, 2),
            );
        }

        // Nerd Font default.
        {
            let constraint = get_constraint(0xea61).unwrap();

            // Verify that this is the constraint we expect.
            assert_eq!(ConstraintSize::FitCover1, constraint.size);
            assert_eq!(ConstraintHeight::Icon, constraint.height);
            assert_eq!(ConstraintAlign::Center1, constraint.align_horizontal);
            assert_eq!(ConstraintAlign::Center1, constraint.align_vertical);

            // BBox of '' (0xEA61 nf-cod-lightbulb) from Symbols Only.
            // NOTE: This icon is part of a group, so the
            // constraint applies to a larger bounding box.
            let glyph_ea61 = GlyphSize {
                width: 9.015625,
                height: 13.015625,
                x: 3.015625,
                y: 3.76525,
            };

            // Constraint width 1: scale and shift group to fit a single cell.
            expect_approx_equal(
                GlyphSize {
                    width: 7.2125,
                    height: 10.4125,
                    x: 0.8125,
                    y: 5.950695224719102,
                },
                constraint.constrain(glyph_ea61, metrics, 1),
            );

            // Constraint width 2: no scaling; left-align and vertically center group.
            expect_approx_equal(
                GlyphSize {
                    width: glyph_ea61.width,
                    height: glyph_ea61.height,
                    x: 1.015625,
                    y: 4.7483690308988775,
                },
                constraint.constrain(glyph_ea61, metrics, 2),
            );
        }

        // Nerd Font stretch.
        {
            let constraint = get_constraint(0xe0c0).unwrap();

            // Verify that this is the constraint we expect.
            assert_eq!(ConstraintSize::Stretch, constraint.size);
            assert_eq!(ConstraintHeight::Cell, constraint.height);
            assert_eq!(ConstraintAlign::Start, constraint.align_horizontal);
            assert_eq!(ConstraintAlign::Center1, constraint.align_vertical);

            // BBox of ' ' (0xE0C0 nf-ple-flame_thick) from Symbols Only.
            let glyph_e0c0 = GlyphSize {
                width: 16.796875,
                height: 16.46875,
                x: -0.796875,
                y: 1.7109375,
            };

            // Constraint width 1: stretch and position to exactly cover one cell.
            expect_approx_equal(
                GlyphSize {
                    width: metrics.cell_width as f64,
                    height: metrics.cell_height as f64,
                    x: 0.0,
                    y: 0.0,
                },
                constraint.constrain(glyph_e0c0, metrics, 1),
            );

            // Constraint width 1: stretch and position to exactly cover two cells.
            expect_approx_equal(
                GlyphSize {
                    width: (2 * metrics.cell_width) as f64,
                    height: metrics.cell_height as f64,
                    x: 0.0,
                    y: 0.0,
                },
                constraint.constrain(glyph_e0c0, metrics, 2),
            );
        }
    }
}
