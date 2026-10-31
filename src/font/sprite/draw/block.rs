//! Port of `src/font/sprite/draw/block.zig`.
//!
//! STATUS: ported
//!
//! Block Elements | U+2580...U+259F
//! https://en.wikipedia.org/wiki/Block_Elements
//!
//! ▀▁▂▃▄▅▆▇█▉▊▋▌▍▎▏
//! ▐░▒▓▔▕▖▗▘▙▚▛▜▝▞▟
//!

use super::common::{
    fill, Alignment, Fraction, HorizontalAlignment, Quads, Shade, VerticalAlignment, FIVE_EIGHTHS,
    HALF, ONE_EIGHTH, ONE_QUARTER, SEVEN_EIGHTHS, THREE_EIGHTHS, THREE_QUARTERS,
};
use crate::font::metrics::Metrics;
use crate::font::sprite::canvas::{Canvas, Rect};
use crate::font::sprite::face::DrawFnError;

pub fn draw2580_259f(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = width;
    let _ = height;

    match cp {
        // '▀' UPPER HALF BLOCK
        0x2580 => block(metrics, canvas, Alignment::UPPER, 1.0, HALF),
        // '▁' LOWER ONE EIGHTH BLOCK
        0x2581 => block(metrics, canvas, Alignment::LOWER, 1.0, ONE_EIGHTH),
        // '▂' LOWER ONE QUARTER BLOCK
        0x2582 => block(metrics, canvas, Alignment::LOWER, 1.0, ONE_QUARTER),
        // '▃' LOWER THREE EIGHTHS BLOCK
        0x2583 => block(metrics, canvas, Alignment::LOWER, 1.0, THREE_EIGHTHS),
        // '▄' LOWER HALF BLOCK
        0x2584 => block(metrics, canvas, Alignment::LOWER, 1.0, HALF),
        // '▅' LOWER FIVE EIGHTHS BLOCK
        0x2585 => block(metrics, canvas, Alignment::LOWER, 1.0, FIVE_EIGHTHS),
        // '▆' LOWER THREE QUARTERS BLOCK
        0x2586 => block(metrics, canvas, Alignment::LOWER, 1.0, THREE_QUARTERS),
        // '▇' LOWER SEVEN EIGHTHS BLOCK
        0x2587 => block(metrics, canvas, Alignment::LOWER, 1.0, SEVEN_EIGHTHS),
        // '█' FULL BLOCK
        0x2588 => full_block_shade(metrics, canvas, Shade::ON),
        // '▉' LEFT SEVEN EIGHTHS BLOCK
        0x2589 => block(metrics, canvas, Alignment::LEFT, SEVEN_EIGHTHS, 1.0),
        // '▊' LEFT THREE QUARTERS BLOCK
        0x258a => block(metrics, canvas, Alignment::LEFT, THREE_QUARTERS, 1.0),
        // '▋' LEFT FIVE EIGHTHS BLOCK
        0x258b => block(metrics, canvas, Alignment::LEFT, FIVE_EIGHTHS, 1.0),
        // '▌' LEFT HALF BLOCK
        0x258c => block(metrics, canvas, Alignment::LEFT, HALF, 1.0),
        // '▍' LEFT THREE EIGHTHS BLOCK
        0x258d => block(metrics, canvas, Alignment::LEFT, THREE_EIGHTHS, 1.0),
        // '▎' LEFT ONE QUARTER BLOCK
        0x258e => block(metrics, canvas, Alignment::LEFT, ONE_QUARTER, 1.0),
        // '▏' LEFT ONE EIGHTH BLOCK
        0x258f => block(metrics, canvas, Alignment::LEFT, ONE_EIGHTH, 1.0),

        // '▐' RIGHT HALF BLOCK
        0x2590 => block(metrics, canvas, Alignment::RIGHT, HALF, 1.0),
        // '░'
        0x2591 => full_block_shade(metrics, canvas, Shade::LIGHT),
        // '▒'
        0x2592 => full_block_shade(metrics, canvas, Shade::MEDIUM),
        // '▓'
        0x2593 => full_block_shade(metrics, canvas, Shade::DARK),
        // '▔' UPPER ONE EIGHTH BLOCK
        0x2594 => block(metrics, canvas, Alignment::UPPER, 1.0, ONE_EIGHTH),
        // '▕' RIGHT ONE EIGHTH BLOCK
        0x2595 => block(metrics, canvas, Alignment::RIGHT, ONE_EIGHTH, 1.0),
        // '▖'
        0x2596 => quadrant(metrics, canvas, Quads { bl: true, ..Default::default() }),
        // '▗'
        0x2597 => quadrant(metrics, canvas, Quads { br: true, ..Default::default() }),
        // '▘'
        0x2598 => quadrant(metrics, canvas, Quads { tl: true, ..Default::default() }),
        // '▙'
        0x2599 => quadrant(
            metrics,
            canvas,
            Quads { tl: true, bl: true, br: true, ..Default::default() },
        ),
        // '▚'
        0x259a => quadrant(metrics, canvas, Quads { tl: true, br: true, ..Default::default() }),
        // '▛'
        0x259b => quadrant(
            metrics,
            canvas,
            Quads { tl: true, tr: true, bl: true, ..Default::default() },
        ),
        // '▜'
        0x259c => quadrant(
            metrics,
            canvas,
            Quads { tl: true, tr: true, br: true, ..Default::default() },
        ),
        // '▝'
        0x259d => quadrant(metrics, canvas, Quads { tr: true, ..Default::default() }),
        // '▞'
        0x259e => quadrant(metrics, canvas, Quads { tr: true, bl: true, ..Default::default() }),
        // '▟'
        0x259f => quadrant(
            metrics,
            canvas,
            Quads { tr: true, bl: true, br: true, ..Default::default() },
        ),

        _ => unreachable!(),
    }
    Ok(())
}

pub fn block(metrics: Metrics, canvas: &mut Canvas, alignment: Alignment, width: f64, height: f64) {
    block_shade(metrics, canvas, alignment, width, height, Shade::ON);
}

pub fn block_shade(
    metrics: Metrics,
    canvas: &mut Canvas,
    alignment: Alignment,
    width: f64,
    height: f64,
    shade: Shade,
) {
    let float_width = metrics.cell_width as f64;
    let float_height = metrics.cell_height as f64;

    let w = (float_width * width).round() as u32;
    let h = (float_height * height).round() as u32;

    let x = match alignment.horizontal {
        HorizontalAlignment::Left => 0,
        HorizontalAlignment::Right => metrics.cell_width - w,
        HorizontalAlignment::Center => (metrics.cell_width - w) / 2,
    };
    let y = match alignment.vertical {
        VerticalAlignment::Top => 0,
        VerticalAlignment::Bottom => metrics.cell_height - h,
        VerticalAlignment::Middle => (metrics.cell_height - h) / 2,
    };

    canvas.rect(
        Rect {
            x: x as i32,
            y: y as i32,
            width: w as i32,
            height: h as i32,
        },
        shade.color(),
    );
}

pub fn full_block_shade(metrics: Metrics, canvas: &mut Canvas, shade: Shade) {
    canvas.r#box(
        0,
        0,
        metrics.cell_width as i32,
        metrics.cell_height as i32,
        shade.color(),
    );
}

fn quadrant(metrics: Metrics, canvas: &mut Canvas, quads: Quads) {
    if quads.tl {
        fill(metrics, canvas, Fraction::Zero, Fraction::Half, Fraction::Zero, Fraction::Half);
    }
    if quads.tr {
        fill(metrics, canvas, Fraction::Half, Fraction::Full, Fraction::Zero, Fraction::Half);
    }
    if quads.bl {
        fill(metrics, canvas, Fraction::Zero, Fraction::Half, Fraction::Half, Fraction::Full);
    }
    if quads.br {
        fill(metrics, canvas, Fraction::Half, Fraction::Full, Fraction::Half, Fraction::Full);
    }
}
