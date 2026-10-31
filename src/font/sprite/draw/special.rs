//! Port of `src/font/sprite/draw/special.zig`.
//!
//! STATUS: ported
//!
//! This file contains glyph drawing functions for all of the
//! non-Unicode sprite glyphs, such as cursors and underlines.
//!
//! The naming convention in this file differs from the usual
//! because the draw functions for special sprites are found by
//! having names that exactly match the enum fields in Sprite.

use crate::font::metrics::Metrics;
use crate::font::sprite::canvas::{CapMode, Canvas, Color, Rect};
use crate::font::sprite::face::DrawFnError;

pub fn underline(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = cp;

    // We can go beyond the height of the cell a bit, but
    // we want to be sure never to exceed the height of the
    // canvas, which extends a quarter cell below the cell
    // height.
    let y = metrics.underline_position.min(
        height
            .saturating_add(canvas.padding_y)
            .saturating_sub(metrics.underline_thickness),
    );

    canvas.rect(
        Rect {
            x: 0,
            y: y as i32,
            width: width as i32,
            height: metrics.underline_thickness as i32,
        },
        Color::ON,
    );
    Ok(())
}

pub fn underline_double(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = cp;

    // We can go beyond the height of the cell a bit, but
    // we want to be sure never to exceed the height of the
    // canvas, which extends a quarter cell below the cell
    // height.
    let y = metrics.underline_position.min(
        height
            .saturating_add(canvas.padding_y)
            .saturating_sub(2 * metrics.underline_thickness),
    );

    // We place one underline above the underline position, and one below
    // by one thickness, creating a "negative" underline where the single
    // underline would be placed.
    canvas.rect(
        Rect {
            x: 0,
            y: y.saturating_sub(metrics.underline_thickness) as i32,
            width: width as i32,
            height: metrics.underline_thickness as i32,
        },
        Color::ON,
    );
    canvas.rect(
        Rect {
            x: 0,
            y: y.saturating_add(metrics.underline_thickness) as i32,
            width: width as i32,
            height: metrics.underline_thickness as i32,
        },
        Color::ON,
    );
    Ok(())
}

pub fn underline_dotted(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = cp;

    let float_width = width as f64;
    let float_height = height as f64;
    let float_pos = metrics.underline_position as f64;
    let float_thick = metrics.underline_thickness as f64;

    // The diameter will be sqrt2 * the usual underline thickness
    // since otherwise dotted underlines look somewhat anemic.
    let radius = std::f64::consts::FRAC_1_SQRT_2 * float_thick;

    // We can go beyond the height of the cell a bit, but
    // we want to be sure never to exceed the height of the
    // canvas, which extends a quarter cell below the cell
    // height.
    let padding = canvas.padding_y as f64;
    let y = (
        // The center of the underline stem.
        float_pos + 0.5 * float_thick
    )
        .min(
            // The lowest we can go on the canvas and not get clipped.
            float_height + padding - radius.ceil(),
        );

    let dot_count: f64 = (
        // We should try to have enough dots that the
        // space between them matches their diameter.
        (float_width / (4.0 * radius)).ceil()
    )
    // And not enough that the space between
    // each dot is less than their radius.
    .min((float_width / (3.0 * radius)).floor())
    // And definitely not enough that the space
    // between them is less than a single pixel.
    .min((float_width / (2.0 * radius + 1.0)).floor())
    // And we must have at least one dot per cell.
    .max(1.0);

    let mut ctx = canvas.get_context();

    // What we essentially do is divide the cell in to
    // dot_count areas with a dot centered in each one.
    let mut x: f64 = (float_width / dot_count) / 2.0;
    for _ in 0..(dot_count as usize) {
        ctx.arc(x, y, radius, 0.0, std::f64::consts::TAU)?;
        ctx.close_path()?;
        x += float_width / dot_count;
    }

    ctx.fill()
}

pub fn underline_dashed(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = cp;

    // We can go beyond the height of the cell a bit, but
    // we want to be sure never to exceed the height of the
    // canvas, which extends a quarter cell below the cell
    // height.
    let y = metrics.underline_position.min(
        height
            .saturating_add(canvas.padding_y)
            .saturating_sub(metrics.underline_thickness),
    );

    let dash_width = width / 3 + 1;
    let dash_count = (width / dash_width) + 1;
    let mut i: u32 = 0;
    while i < dash_count {
        let x = i * dash_width;
        canvas.rect(
            Rect {
                x: x as i32,
                y: y as i32,
                width: dash_width as i32,
                height: metrics.underline_thickness as i32,
            },
            Color::ON,
        );
        i += 2;
    }
    Ok(())
}

pub fn underline_curly(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = cp;

    let padding = canvas.padding_y as f64;
    let mut ctx = canvas.get_context();

    let float_width = width as f64;
    let float_height = height as f64;
    let float_pos = metrics.underline_position as f64;

    // Because of we way we draw the undercurl, we end up making it around 1px
    // thicker than it should be, to fix this we just reduce the thickness by 1.
    //
    // We use a minimum thickness of 0.414 because this empirically produces
    // the nicest undercurls at 1px underline thickness; thinner tends to look
    // too thin compared to straight underlines and has artefacting.
    ctx.line_width = metrics.underline_thickness as f64;

    // Rounded caps, adjacent underlines will have these overlap and so not be
    // visible, but it makes the ends look cleaner.
    ctx.line_cap_mode = CapMode::Round;

    // Empirically this looks good.
    let amplitude = float_width / std::f64::consts::PI;

    // Make sure we don't exceed the drawable area. This can still be outside
    // of the cell by some amount (one quarter of the height), but we don't
    // want underlines to disappear for fonts with bad metadata or when users
    // set their underline position way too low.
    let top: f64 = float_pos.min(
        // The lowest we can draw this and not get clipped.
        float_height + padding - amplitude - ctx.line_width,
    );
    let bottom = top + amplitude;

    // Curvature multiplier.
    // To my eye, 0.4 creates a nice smooth wiggle.
    let r = 0.4;

    let center = 0.5 * float_width;

    // We create a single cycle of a wave that peaks at the center of the cell.
    ctx.move_to(0.0, bottom)?;
    ctx.curve_to(center * r, bottom, center - center * r, top, center, top)?;
    ctx.curve_to(
        center + center * r,
        top,
        float_width - center * r,
        bottom,
        float_width,
        bottom,
    )?;
    ctx.stroke()
}

pub fn strikethrough(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = cp;
    let _ = height;

    canvas.rect(
        Rect {
            x: 0,
            y: metrics.strikethrough_position as i32,
            width: width as i32,
            height: metrics.strikethrough_thickness as i32,
        },
        Color::ON,
    );
    Ok(())
}

pub fn overline(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = cp;
    let _ = height;

    // We can go beyond the top of the cell a bit, but we
    // want to be sure never to exceed the height of the
    // canvas, which extends a quarter cell above the top
    // of the cell.
    let y = metrics.overline_position.max(-(canvas.padding_y as i32));

    canvas.rect(
        Rect {
            x: 0,
            y,
            width: width as i32,
            height: metrics.overline_thickness as i32,
        },
        Color::ON,
    );
    Ok(())
}

pub fn cursor_rect(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = cp;
    let _ = metrics;

    canvas.rect(
        Rect {
            x: 0,
            y: 0,
            width: width as i32,
            height: height as i32,
        },
        Color::ON,
    );
    Ok(())
}

pub fn cursor_hollow_rect(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = cp;

    // We fill the entire rect and then hollow out the inside, this isn't very
    // efficient but it doesn't need to be and it's the easiest way to write it.
    canvas.rect(
        Rect {
            x: 0,
            y: 0,
            width: width as i32,
            height: height as i32,
        },
        Color::ON,
    );
    canvas.rect(
        Rect {
            x: metrics.cursor_thickness as i32,
            y: metrics.cursor_thickness as i32,
            width: width.saturating_sub(metrics.cursor_thickness * 2) as i32,
            height: height.saturating_sub(metrics.cursor_thickness * 2) as i32,
        },
        Color::OFF,
    );
    Ok(())
}

pub fn cursor_bar(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = cp;
    let _ = width;

    // We place the bar cursor half of its thickness over the left edge of the
    // cell, so that it sits centered between characters, not biased to a side.
    //
    // We round up (add 1 before dividing by 2) because, empirically, having a
    // 1px cursor shifted left a pixel looks better than having it not shifted.
    canvas.rect(
        Rect {
            x: -(((metrics.cursor_thickness + 1) / 2) as i32),
            y: 0,
            width: metrics.cursor_thickness as i32,
            height: height as i32,
        },
        Color::ON,
    );
    Ok(())
}

pub fn cursor_underline(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = cp;

    // We can go beyond the height of the cell a bit, but
    // we want to be sure never to exceed the height of the
    // canvas, which extends a quarter cell below the cell
    // height.
    let y = metrics.underline_position.min(
        height
            .saturating_add(canvas.padding_y)
            .saturating_sub(metrics.underline_thickness),
    );

    canvas.rect(
        Rect {
            x: 0,
            y: y as i32,
            width: width as i32,
            height: metrics.cursor_thickness as i32,
        },
        Color::ON,
    );
    Ok(())
}
