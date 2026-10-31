//! Port of `src/font/glyf_rasterize.zig`.
//!
//! STATUS: ported
//!
//! Rasterization for OpenType glyf outlines.
//!
//! This module intentionally lives in `font` rather than `font/opentype`
//! because I wanted to keep `font/opentype` dependency free on the font
//! package.
//!
//! Port note: the Zig code fills the path with the `z2d` library. This
//! port uses the z2d-shaped drawing layer of the sprite canvas
//! (`crate::font::sprite::canvas::z2d`), whose anti-aliased coverage can
//! differ slightly from z2d's 4x multisampling.

use super::glyph::{self, DesignMetrics, RenderOptions};
use super::opentype::glyf;
use super::sprite::canvas::z2d;
use crate::quirks::inline_assert as assert;

/// An owned, tightly packed alpha8 bitmap.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl Bitmap {
    // An empty 0x0 bitmap.
    pub const EMPTY: Bitmap = Bitmap {
        width: 0,
        height: 0,
        data: Vec::new(),
    };

    pub fn init_empty(width: u32, height: u32) -> Bitmap {
        let data = vec![0u8; width as usize * height as usize];
        Bitmap {
            width,
            height,
            data,
        }
    }
}

/// Zig `Allocator.Error || z2d.Path.Error || z2d.painter.FillError`.
pub type Error = super::sprite::canvas::DrawError;

/// Rasterize a decoded glyf outline to a full-cell alpha bitmap.
///
/// The returned bitmap is always `grid_metrics.cell_width * cell_width` by
/// `grid_metrics.cell_height`. `opts.constraint` is applied using the same
/// `RenderOptions.Constraint` machinery used by the platform font
/// backends.
///
/// The caller owns the returned bitmap.
pub fn rasterize(
    outline: &glyf::Outline,
    design: DesignMetrics,
    opts: RenderOptions,
) -> Result<Bitmap, Error> {
    assert(design.units_per_em > 0);
    assert(design.advance_width > 0);
    assert(design.line_height > 0);

    // Calculate our final width/height.
    let width: u32 = opts
        .grid_metrics
        .cell_width
        .checked_mul(opts.cell_width.unwrap_or(1) as u32)
        .unwrap_or(u32::MAX);
    let height = opts.grid_metrics.cell_height;
    assert(width > 0 && height > 0);

    // If we have no contours or points then we have no drawable shape, but the
    // caller still asked for a cell-sized bitmap. Return that full bitmap with
    // zero coverage so downstream atlas/upload code doesn't need a separate
    // size contract for empty glyphs.
    if outline.contours.is_empty() || outline.points.is_empty() {
        return Ok(Bitmap::init_empty(width, height));
    }

    // Glyf entries have a header bounding box, but this rasterizer operates on
    // the decoded Outline only. Recompute bounds from the decoded coordinate
    // data so placement and scaling follow the geometry we actually draw. If a
    // source glyf header disagrees with its points, the point data is the safer
    // source of truth for rasterization; the header belongs in decode-time
    // validation/metadata, not this font-level drawing API.
    let bounds: Bounds = {
        let first = outline.points[0];
        let mut bounds = Bounds {
            x_min: first.x as f64,
            y_min: first.y as f64,
            x_max: first.x as f64,
            y_max: first.y as f64,
        };
        for p in &outline.points[1..] {
            let x = p.x as f64;
            let y = p.y as f64;
            bounds.x_min = bounds.x_min.min(x);
            bounds.y_min = bounds.y_min.min(y);
            bounds.x_max = bounds.x_max.max(x);
            bounds.y_max = bounds.y_max.max(y);
        }
        bounds
    };

    // Degenerate point bounds can't produce filled area and would make the
    // point-to-bitmap transform divide by zero, so return a full transparent
    // bitmap just like an empty outline.
    if bounds.width() == 0.0 || bounds.height() == 0.0 {
        return Ok(Bitmap::init_empty(width, height));
    }

    // Build the surface directly over the bitmap we'll return.
    let mut sfc = z2d::Surface::init(width as i32, height as i32);

    let mut path = z2d::Path::default();

    let placement = Placement::init(bounds, design, opts);
    for i in 0..outline.contours.len() {
        append_contour_path(&mut path, outline.contour(i), bounds, placement)?;
    }

    z2d::painter::fill(&mut sfc, 255, &path.nodes, &z2d::FillOptions::default())?;

    Ok(Bitmap {
        width,
        height,
        data: sfc.buf,
    })
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Bounds {
    x_min: f64,
    y_min: f64,
    x_max: f64,
    y_max: f64,
}

impl Bounds {
    fn width(self) -> f64 {
        self.x_max - self.x_min
    }

    fn height(self) -> f64 {
        self.y_max - self.y_min
    }
}

/// Cell-relative pixel rectangle where the decoded outline bounds should be
/// rasterized within the output bitmap.
///
/// This is deliberately the placement of the outline's computed point bounds,
/// not the full declared advance/line-height box. `advance_width` and
/// `line_height` describe the design-space layout box the outline was drawn
/// within: they include intentional bearings and whitespace around the visible
/// points. We use that declared box when applying `RenderOptions.Constraint` so
/// sizing and alignment preserve those bearings consistently with other font
/// backends; once that is resolved, we rasterize only the actual outline bounds
/// into this rectangle.
///
/// ```text
/// output bitmap / terminal cell
/// ╭────────────────────────────────────────────────────────────────────────╮ top
/// │                                                                        │
/// │   declared advance/line-height box                                     │
/// │   (outer layout box used for constraints)                              │
/// │   ╭────────────────────────────────────────────────────────────────╮   │
/// │   │                                                                │   │
/// │◀──────── x ────────▶╭────────── width ──────────╮                  │   │
/// │   │                 │ Placement                 │ ▲ height         │   │
/// │   │                 │ outline point bounds      │ │                │   │
/// │   │                 │ pixels to draw            │ │                │   │
/// │   │                 │                           │ │                │   │
/// │   │                 ╰───────────────────────────╯ ▼                │   │
/// │   ╰─────────────────▲──────────────────────────────────────────────╯   │
/// │                     │ y                                                │
/// ╰────────────────────────────────────────────────────────────────────────╯ bottom
///              x is measured from the bitmap left to the Placement left.
///              y is measured from the bitmap bottom to the Placement bottom.
///              bitmap_height is the full top-to-bottom bitmap height.
/// ```
///
/// Constraints are applied to the outer box so the whitespace remains part of
/// alignment decisions. `Placement` is the inner rectangle after that outer box
/// has been constrained.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Placement {
    /// Left edge of the rasterized outline bounds in bitmap pixels, measured
    /// from the bitmap's left edge.
    x: f64,

    /// Bottom edge of the rasterized outline bounds in bitmap pixels, measured
    /// from the bitmap's bottom edge. This matches the cell-relative y axis
    /// used by font.Glyph.Size and is converted to z2d's y-down axis when
    /// points are transformed.
    y: f64,

    /// Width of the rasterized outline bounds in bitmap pixels after applying
    /// font.Glyph.RenderOptions.Constraint.
    width: f64,

    /// Height of the rasterized outline bounds in bitmap pixels after applying
    /// font.Glyph.RenderOptions.Constraint.
    height: f64,

    /// Full bitmap height in pixels, used to convert cell-relative y-up-ish
    /// placement into the y-down coordinate system used by z2d surfaces.
    bitmap_height: f64,
}

impl Placement {
    /// Calculate where the decoded point bounds should land in the output
    /// bitmap.
    ///
    /// The glyf protocol supplies declared metrics (`units_per_em`,
    /// `advance_width`, and `line_height`) in design units, while Ghostty's
    /// font constraint code works in cell-relative pixels. We first map the em
    /// square to one cell height, matching the linked glyph rasterizer's
    /// baseline model where design-space `y=0` is the bottom/baseline of the em
    /// and `y=units_per_em` is its top. Then we describe the actual outline
    /// bounds as a relative sub-rectangle of the declared advance/line-height
    /// box. That declared box includes any intentional side bearings or
    /// vertical whitespace around the outline; constraints should apply to that
    /// layout box rather than to the tight point bounds alone. This returns the
    /// final pixel rectangle for only the outline bounds that we will rasterize.
    fn init(bounds: Bounds, design: DesignMetrics, opts: RenderOptions) -> Placement {
        // Start with protocol-like design units mapped so that the em square
        // occupies one cell. This makes units_per_em the scale reference and
        // preserves the linked rasterizer's y=0 baseline/bottom behavior.
        // Callers can then use RenderOptions.Constraint to fit/cover/stretch/
        // align the declared advance/line-height box using existing font logic.
        let scale = opts.grid_metrics.cell_height as f64 / design.units_per_em as f64;

        // Convert the decoded point bounds into the same pixel coordinate space
        // expected by RenderOptions.Constraint. This rectangle is the visible
        // outline bounds, not the full advance/line-height layout box.
        let glyph = glyph::Size {
            width: bounds.width() * scale,
            height: bounds.height() * scale,
            x: bounds.x_min * scale,
            y: bounds.y_min * scale,
        };

        // Convert the declared layout box to pixels. This is the box that
        // carries intentional bearings/whitespace and should be constrained.
        let group_width = design.advance_width as f64 * scale;
        let group_height = design.line_height as f64 * scale;

        // Apply the same fit/cover/stretch/alignment/padding rules used by
        // normal font rendering. The result is still the outline bounds, but
        // placed as if its containing advance/line-height box was constrained.
        let constraint = {
            let mut constraint = opts.constraint;
            if group_width > 0.0 && group_height > 0.0 {
                // Tell Constraint that `glyph` is a sub-rectangle of the
                // declared layout box. Constraint will size/align the outer box
                // and then return the corresponding transformed inner box.
                constraint.relative_width = glyph.width / group_width;
                constraint.relative_height = glyph.height / group_height;
                constraint.relative_x = glyph.x / group_width;
                constraint.relative_y = glyph.y / group_height;
            }
            constraint
        };
        let constrained = constraint.constrain(glyph, opts.grid_metrics, opts.constraint_width);

        // Store the final outline placement plus the full bitmap height needed
        // later to flip from cell-relative y to z2d's y-down surface space.
        Placement {
            x: constrained.x,
            y: constrained.y,
            width: constrained.width,
            height: constrained.height,
            bitmap_height: opts.grid_metrics.cell_height as f64,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Point {
    x: f64,
    y: f64,
}

/// Append one contour to a z2d path.
///
/// Glyf contours are quadratic outlines with explicit on-curve points and
/// off-curve control points. Consecutive off-curve points imply an on-curve
/// point halfway between them, and a contour may begin with an off-curve point.
/// This normalizes those cases while walking the closed contour and emits z2d
/// line/cubic-curve operations in bitmap coordinates.
fn append_contour_path(
    path: &mut z2d::Path,
    contour: &[glyf::Point],
    bounds: Bounds,
    placement: Placement,
) -> Result<(), Error> {
    if contour.is_empty() {
        return Ok(());
    }

    let first = contour[0];
    let last = contour[contour.len() - 1];

    let mut current: Point;
    let mut i: usize = 0;

    // Choose the starting on-curve point for this closed contour. If the first
    // point is off-curve then the contour logically starts either at the final
    // on-curve point, or at the implied midpoint between the final and first
    // off-curve points.
    if first.on_curve {
        i = 1;
        current = transform_point(first, bounds, placement);
    } else if last.on_curve {
        current = transform_point(last, bounds, placement);
    } else {
        current = midpoint(
            transform_point(last, bounds, placement),
            transform_point(first, bounds, placement),
        );
    }

    // Move to the beginning
    path.move_to(current.x, current.y);

    // Go through the points and connect em!
    while i < contour.len() {
        let p = contour[i];

        // On-curve points connect to the current point with a straight line.
        if p.on_curve {
            current = transform_point(p, bounds, placement);
            path.line_to(current.x, current.y);
            i += 1;
            continue;
        }

        // Off-curve points are quadratic control points. The following point is
        // either the curve endpoint or, if it is also off-curve, contributes an
        // implied on-curve endpoint halfway between the two controls.
        let control = transform_point(p, bounds, placement);
        let next = contour[(i + 1) % contour.len()];
        let end = if next.on_curve {
            transform_point(next, bounds, placement)
        } else {
            midpoint(control, transform_point(next, bounds, placement))
        };

        // z2d paths only expose cubic curves, so convert the TrueType
        // quadratic segment to an equivalent cubic segment before appending it.
        let c1 = Point {
            x: current.x + ((2.0 / 3.0) * (control.x - current.x)),
            y: current.y + ((2.0 / 3.0) * (control.y - current.y)),
        };
        let c2 = Point {
            x: end.x + ((2.0 / 3.0) * (control.x - end.x)),
            y: end.y + ((2.0 / 3.0) * (control.y - end.y)),
        };
        path.curve_to(c1.x, c1.y, c2.x, c2.y, end.x, end.y);

        current = end;

        // If we consumed an explicit on-curve endpoint then skip it; otherwise
        // the next off-curve point still needs to be used as the control point
        // for the following quadratic segment.
        i += if next.on_curve { 2 } else { 1 };
    }

    path.close();
    Ok(())
}

/// Convert a decoded glyf point from design-space coordinates to z2d bitmap
/// coordinates.
///
/// `bounds` describes the decoded outline's point/control bounds in glyf
/// design units. `placement` describes where those bounds should land in the
/// output bitmap after constraints are applied. Glyf coordinates are y-up; z2d
/// surfaces are y-down, so this also flips the y axis using
/// `placement.bitmap_height`.
fn transform_point(p: glyf::Point, bounds: Bounds, placement: Placement) -> Point {
    let scale_x = placement.width / bounds.width();
    let scale_y = placement.height / bounds.height();
    let x_design = p.x as f64;
    let y_design = p.y as f64;
    Point {
        x: placement.x + ((x_design - bounds.x_min) * scale_x),
        y: placement.bitmap_height - placement.y - ((y_design - bounds.y_min) * scale_y),
    }
}

/// Return the implied on-curve point between two off-curve TrueType control
/// points.
fn midpoint(a: Point, b: Point) -> Point {
    Point {
        x: (a.x + b.x) / 2.0,
        y: (a.y + b.y) / 2.0,
    }
}

#[cfg(test)]
pub(crate) fn test_metrics(width: u32, height: u32) -> super::metrics::Metrics {
    super::metrics::Metrics {
        cell_width: width,
        cell_height: height,
        cell_baseline: 0,
        underline_position: height,
        underline_thickness: 1,
        strikethrough_position: height / 2,
        strikethrough_thickness: 1,
        overline_position: 0,
        overline_thickness: 1,
        box_thickness: 1,
        cursor_thickness: 1,
        cursor_height: height,
        icon_height: height as f64,
        icon_height_single: height as f64,
        face_width: width as f64,
        face_height: height as f64,
        face_y: 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::font::glyph::{Constraint, ConstraintAlign, ConstraintSize};

    // Zig: `test { _ = @import("glyf_rasterize_png_test.zig"); }` — the PNG
    // test is its own module (`crate::font::glyf_rasterize_png_test`).

    fn pt(x: i32, y: i32, on_curve: bool) -> glyf::Point {
        glyf::Point { x, y, on_curve }
    }

    fn outline(points: &[glyf::Point], contours: &[u16]) -> glyf::Outline {
        glyf::Outline {
            points: points.to_vec(),
            contours: contours.to_vec(),
        }
    }

    fn design(units_per_em: u32, advance_width: u32, line_height: u32) -> DesignMetrics {
        DesignMetrics {
            units_per_em,
            advance_width,
            line_height,
        }
    }

    fn square() -> glyf::Outline {
        outline(
            &[
                pt(0, 0, true),
                pt(1000, 0, true),
                pt(1000, 1000, true),
                pt(0, 1000, true),
            ],
            &[3],
        )
    }

    fn any_nonzero(bm: &Bitmap) -> bool {
        bm.data.iter().any(|&v| v != 0)
    }

    #[test]
    fn empty_outline_returns_empty_bitmap() {
        let bm = rasterize(
            &outline(&[], &[]),
            design(1000, 1000, 1000),
            RenderOptions::new(test_metrics(20, 20)),
        )
        .unwrap();

        assert_eq!(20u32, bm.width);
        assert_eq!(20u32, bm.height);
        assert_eq!(20 * 20, bm.data.len());
        for &v in &bm.data {
            assert_eq!(0u8, v);
        }
    }

    #[test]
    fn square_fills_bitmap_center() {
        let bm = rasterize(
            &square(),
            design(1000, 1000, 1000),
            RenderOptions::new(test_metrics(20, 20)),
        )
        .unwrap();

        assert!(bm.data[(10 * bm.width + 10) as usize] > 200);
    }

    #[test]
    fn quadratic_contour_renders() {
        let o = outline(
            &[pt(0, 0, true), pt(500, 1000, false), pt(1000, 0, true)],
            &[2],
        );

        let bm = rasterize(&o, design(1000, 1000, 1000), RenderOptions::new(test_metrics(20, 20)))
            .unwrap();

        assert!(any_nonzero(&bm));
    }

    #[test]
    fn consecutive_off_curve_points_render() {
        let o = outline(
            &[
                pt(0, 0, true),
                pt(250, 1000, false),
                pt(750, 1000, false),
                pt(1000, 0, true),
            ],
            &[3],
        );

        let bm = rasterize(&o, design(1000, 1000, 1000), RenderOptions::new(test_metrics(20, 20)))
            .unwrap();

        assert!(any_nonzero(&bm));
    }

    #[test]
    fn units_per_em_controls_baseline_scale() {
        let bm = rasterize(
            &square(),
            design(2000, 1000, 1000),
            RenderOptions::new(test_metrics(20, 20)),
        )
        .unwrap();

        // With a 2000-unit em in a 20px cell, this 1000-unit square occupies the
        // bottom half of the cell. This matches the linked rasterizer's y=0
        // baseline/bottom behavior and proves units_per_em is the scale reference.
        assert!(bm.data[(15 * bm.width + 5) as usize] > 200);
        assert_eq!(0u8, bm.data[(5 * bm.width + 5) as usize]);
    }

    #[test]
    fn degenerate_outline_returns_full_empty_bitmap() {
        let o = outline(
            &[pt(0, 0, true), pt(1000, 0, true), pt(500, 0, true)],
            &[2],
        );

        let bm = rasterize(&o, design(1000, 1000, 1000), RenderOptions::new(test_metrics(20, 20)))
            .unwrap();

        assert_eq!(20u32, bm.width);
        assert_eq!(20u32, bm.height);
        assert_eq!(20 * 20, bm.data.len());
        for &v in &bm.data {
            assert_eq!(0u8, v);
        }
    }

    #[test]
    fn contour_can_start_off_curve_with_final_on_curve_point() {
        let o = outline(
            &[pt(500, 1000, false), pt(1000, 0, true), pt(0, 0, true)],
            &[2],
        );

        let bm = rasterize(&o, design(1000, 1000, 1000), RenderOptions::new(test_metrics(20, 20)))
            .unwrap();

        assert!(any_nonzero(&bm));
    }

    #[test]
    fn contour_can_start_with_implied_midpoint() {
        let o = outline(
            &[
                pt(250, 1000, false),
                pt(1000, 0, true),
                pt(750, 1000, false),
                pt(0, 0, true),
            ],
            &[3],
        );

        let bm = rasterize(&o, design(1000, 1000, 1000), RenderOptions::new(test_metrics(20, 20)))
            .unwrap();

        assert!(any_nonzero(&bm));
    }

    #[test]
    fn multiple_contours_render_independently() {
        let o = outline(
            &[
                pt(0, 0, true),
                pt(400, 0, true),
                pt(400, 400, true),
                pt(0, 400, true),
                pt(600, 600, true),
                pt(1000, 600, true),
                pt(1000, 1000, true),
                pt(600, 1000, true),
            ],
            &[3, 7],
        );

        let bm = rasterize(&o, design(1000, 1000, 1000), RenderOptions::new(test_metrics(20, 20)))
            .unwrap();

        assert!(bm.data[(16 * bm.width + 4) as usize] > 200);
        assert!(bm.data[(4 * bm.width + 16) as usize] > 200);
        assert_eq!(0u8, bm.data[(10 * bm.width + 10) as usize]);
    }

    #[test]
    fn non_zero_bearings_preserve_declared_whitespace() {
        let o = outline(
            &[
                pt(250, 0, true),
                pt(750, 0, true),
                pt(750, 1000, true),
                pt(250, 1000, true),
            ],
            &[3],
        );

        let bm = rasterize(&o, design(1000, 1000, 1000), RenderOptions::new(test_metrics(20, 20)))
            .unwrap();

        assert_eq!(0u8, bm.data[(10 * bm.width + 2) as usize]);
        assert!(bm.data[(10 * bm.width + 10) as usize] > 200);
        assert_eq!(0u8, bm.data[(10 * bm.width + 17) as usize]);
    }

    #[test]
    fn negative_y_coordinates_descend_below_baseline() {
        let o = outline(
            &[
                pt(0, -250, true),
                pt(1000, -250, true),
                pt(1000, 750, true),
                pt(0, 750, true),
            ],
            &[3],
        );

        let bm = rasterize(&o, design(1000, 1000, 1000), RenderOptions::new(test_metrics(20, 20)))
            .unwrap();

        assert_eq!(0u8, bm.data[(2 * bm.width + 10) as usize]);
        assert!(bm.data[(10 * bm.width + 10) as usize] > 200);
        assert!(bm.data[(18 * bm.width + 10) as usize] > 200);
    }

    #[test]
    fn two_cell_bitmap_and_constraint_render_within_width() {
        let bm = rasterize(
            &square(),
            design(1000, 1000, 1000),
            RenderOptions {
                cell_width: Some(2),
                constraint_width: 2,
                constraint: Constraint {
                    size: ConstraintSize::Cover,
                    align_horizontal: ConstraintAlign::Center,
                    align_vertical: ConstraintAlign::Center,
                    ..Constraint::NONE
                },
                ..RenderOptions::new(test_metrics(20, 20))
            },
        )
        .unwrap();

        assert_eq!(40u32, bm.width);
        assert_eq!(20u32, bm.height);
        assert_eq!(0u8, bm.data[(10 * bm.width + 2) as usize]);
        assert!(bm.data[(10 * bm.width + 20) as usize] > 200);
        assert_eq!(0u8, bm.data[(10 * bm.width + 37) as usize]);
    }

    #[test]
    fn line_height_does_not_change_unconstrained_em_scale() {
        let bm = rasterize(
            &square(),
            design(1000, 1000, 2000),
            RenderOptions::new(test_metrics(20, 20)),
        )
        .unwrap();

        assert!(bm.data[(10 * bm.width + 10) as usize] > 200);
        assert!(bm.data[(2 * bm.width + 10) as usize] > 200);
        assert!(bm.data[(17 * bm.width + 10) as usize] > 200);
    }
}
