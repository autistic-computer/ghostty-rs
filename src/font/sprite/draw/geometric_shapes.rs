//! Port of `src/font/sprite/draw/geometric_shapes.zig`.
//!
//! STATUS: ported
//!
//! Geometric Shapes | U+25A0...U+25FF
//! https://en.wikipedia.org/wiki/Geometric_Shapes_(Unicode_block)
//!
//! ■ □ ▢ ▣ ▤ ▥ ▦ ▧ ▨ ▩ ▪ ▫ ▬ ▭ ▮ ▯
//! ▰ ▱ ▲ △ ▴ ▵ ▶ ▷ ▸ ▹ ► ▻ ▼ ▽ ▾ ▿
//! ◀ ◁ ◂ ◃ ◄ ◅ ◆ ◇ ◈ ◉ ◊ ○ ◌ ◍ ◎ ●
//! ◐ ◑ ◒ ◓ ◔ ◕ ◖ ◗ ◘ ◙ ◚ ◛ ◜ ◝ ◞ ◟
//! ◠ ◡ ◢ ◣ ◤ ◥ ◦ ◧ ◨ ◩ ◪ ◫ ◬ ◭ ◮ ◯
//! ◰ ◱ ◲ ◳ ◴ ◵ ◶ ◷ ◸ ◹ ◺ ◻ ◼ ◽︎◾︎◿
//!
//! Only a subset of this block is viable for sprite drawing; filling
//! out this file to have full coverage of this block is not the goal.
//!

use super::common::{Corner, Shade, Thickness};
use crate::font::metrics::Metrics;
use crate::font::sprite::canvas::{CapMode, Canvas, Color, FillOptions, StrokeOptions};
use crate::font::sprite::face::DrawFnError;

/// ◢ ◣ ◤ ◥
pub fn draw25e2_25e5(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = width;
    let _ = height;
    match cp {
        // ◢
        0x25e2 => corner_triangle_shade(metrics, canvas, Corner::Br, Shade::ON)?,
        // ◣
        0x25e3 => corner_triangle_shade(metrics, canvas, Corner::Bl, Shade::ON)?,
        // ◤
        0x25e4 => corner_triangle_shade(metrics, canvas, Corner::Tl, Shade::ON)?,
        // ◥
        0x25e5 => corner_triangle_shade(metrics, canvas, Corner::Tr, Shade::ON)?,

        _ => unreachable!(),
    }
    Ok(())
}

/// ◸ ◹ ◺
pub fn draw25f8_25fa(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = width;
    let _ = height;
    match cp {
        // ◸
        0x25f8 => corner_triangle_outline(metrics, canvas, Corner::Tl)?,
        // ◹
        0x25f9 => corner_triangle_outline(metrics, canvas, Corner::Tr)?,
        // ◺
        0x25fa => corner_triangle_outline(metrics, canvas, Corner::Bl)?,

        _ => unreachable!(),
    }
    Ok(())
}

/// ◿
pub fn draw25ff(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = cp;
    let _ = width;
    let _ = height;
    corner_triangle_outline(metrics, canvas, Corner::Br)
}

/// The triangle vertices `(x0, y0, x1, y1, x2, y2)` for a corner.
fn corner_triangle(metrics: Metrics, corner: Corner) -> (f64, f64, f64, f64, f64, f64) {
    let float_width = metrics.cell_width as f64;
    let float_height = metrics.cell_height as f64;

    match corner {
        Corner::Tl => (0.0, 0.0, 0.0, float_height, float_width, 0.0),
        Corner::Tr => (0.0, 0.0, float_width, float_height, float_width, 0.0),
        Corner::Bl => (0.0, 0.0, 0.0, float_height, float_width, float_height),
        Corner::Br => (0.0, float_height, float_width, float_height, float_width, 0.0),
    }
}

pub fn corner_triangle_shade(
    metrics: Metrics,
    canvas: &mut Canvas,
    corner: Corner,
    shade: Shade,
) -> Result<(), DrawFnError> {
    let (x0, y0, x1, y1, x2, y2) = corner_triangle(metrics, corner);

    let mut path = canvas.static_path(5); // nodes.len = 0
    path.move_to(x0, y0); // +1, nodes.len = 1
    path.line_to(x1, y1); // +1, nodes.len = 2
    path.line_to(x2, y2); // +1, nodes.len = 3
    path.close(); // +2, nodes.len = 5

    canvas.fill_path(&path, FillOptions::default(), shade.color())
}

pub fn corner_triangle_outline(
    metrics: Metrics,
    canvas: &mut Canvas,
    corner: Corner,
) -> Result<(), DrawFnError> {
    let float_thick = Thickness::Light.height(metrics.box_thickness) as f64;
    let (x0, y0, x1, y1, x2, y2) = corner_triangle(metrics, corner);

    let mut path = canvas.static_path(5); // nodes.len = 0
    path.move_to(x0, y0); // +1, nodes.len = 1
    path.line_to(x1, y1); // +1, nodes.len = 2
    path.line_to(x2, y2); // +1, nodes.len = 3
    path.close(); // +2, nodes.len = 5

    canvas.inner_stroke_path(
        &path,
        StrokeOptions {
            line_cap_mode: CapMode::Butt,
            line_width: float_thick,
            ..StrokeOptions::default()
        },
        Color::ON,
    )
}
