//! Port of `src/font/sprite/draw/powerline.zig`.
//!
//! STATUS: ported
//!
//! Powerline + Powerline Extra Symbols | U+E0B0...U+E0D4
//! https://github.com/ryanoasis/powerline-extra-symbols
//!
//!                
//!                  
//!      
//!
//! We implement the more geometric glyphs here, but not the stylized ones.
//!

use super::common::Thickness;
use super::r#box;
use crate::font::metrics::Metrics;
use crate::font::sprite::canvas::{
    CapMode, Canvas, Color, FillOptions, Point, StrokeOptions, Triangle,
};
use crate::font::sprite::face::DrawFnError;

/// 
pub fn drawe0b0(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = cp;
    let _ = metrics;
    let float_width = width as f64;
    let float_height = height as f64;
    canvas.triangle(
        Triangle {
            p0: Point { x: 0.0, y: 0.0 },
            p1: Point { x: float_width, y: float_height / 2.0 },
            p2: Point { x: 0.0, y: float_height },
        },
        Color::ON,
    )
}

/// 
pub fn drawe0b2(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = cp;
    let _ = metrics;
    let float_width = width as f64;
    let float_height = height as f64;
    canvas.triangle(
        Triangle {
            p0: Point { x: float_width, y: 0.0 },
            p1: Point { x: 0.0, y: float_height / 2.0 },
            p2: Point { x: float_width, y: float_height },
        },
        Color::ON,
    )
}

/// 
pub fn drawe0b8(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = cp;
    let _ = metrics;
    let float_width = width as f64;
    let float_height = height as f64;
    canvas.triangle(
        Triangle {
            p0: Point { x: 0.0, y: 0.0 },
            p1: Point { x: float_width, y: float_height },
            p2: Point { x: 0.0, y: float_height },
        },
        Color::ON,
    )
}

/// 
pub fn drawe0b9(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = cp;
    let _ = width;
    let _ = height;
    r#box::light_diagonal_upper_left_to_lower_right(metrics, canvas);
    Ok(())
}

/// 
pub fn drawe0ba(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = cp;
    let _ = metrics;
    let float_width = width as f64;
    let float_height = height as f64;
    canvas.triangle(
        Triangle {
            p0: Point { x: float_width, y: 0.0 },
            p1: Point { x: float_width, y: float_height },
            p2: Point { x: 0.0, y: float_height },
        },
        Color::ON,
    )
}

/// 
pub fn drawe0bb(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = cp;
    let _ = width;
    let _ = height;
    r#box::light_diagonal_upper_right_to_lower_left(metrics, canvas);
    Ok(())
}

/// 
pub fn drawe0bc(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = cp;
    let _ = metrics;
    let float_width = width as f64;
    let float_height = height as f64;
    canvas.triangle(
        Triangle {
            p0: Point { x: 0.0, y: 0.0 },
            p1: Point { x: float_width, y: 0.0 },
            p2: Point { x: 0.0, y: float_height },
        },
        Color::ON,
    )
}

/// 
pub fn drawe0bd(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = cp;
    let _ = width;
    let _ = height;
    r#box::light_diagonal_upper_right_to_lower_left(metrics, canvas);
    Ok(())
}

/// 
pub fn drawe0be(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = cp;
    let _ = metrics;
    let float_width = width as f64;
    let float_height = height as f64;
    canvas.triangle(
        Triangle {
            p0: Point { x: 0.0, y: 0.0 },
            p1: Point { x: float_width, y: 0.0 },
            p2: Point { x: float_width, y: float_height },
        },
        Color::ON,
    )
}

/// 
pub fn drawe0bf(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = cp;
    let _ = width;
    let _ = height;
    r#box::light_diagonal_upper_left_to_lower_right(metrics, canvas);
    Ok(())
}

/// 
pub fn drawe0b1(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = cp;
    let float_width = width as f64;
    let float_height = height as f64;

    let mut path = canvas.static_path(3);
    path.move_to(0.0, 0.0);
    path.line_to(float_width, float_height / 2.0);
    path.line_to(0.0, float_height);

    canvas.stroke_path(
        &path,
        StrokeOptions {
            line_cap_mode: CapMode::Butt,
            line_width: Thickness::Light.height(metrics.box_thickness) as f64,
            ..StrokeOptions::default()
        },
        Color::ON,
    )
}

/// 
pub fn drawe0b3(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    drawe0b1(cp, canvas, width, height, metrics)?;
    canvas.flip_horizontal()
}

/// 
pub fn drawe0b4(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = cp;
    let _ = metrics;

    let float_width = width as f64;
    let float_height = height as f64;

    // Coefficient for approximating a circular arc.
    let c: f64 = (std::f64::consts::SQRT_2 - 1.0) * 4.0 / 3.0;

    let radius: f64 = float_width.min(float_height / 2.0);

    let mut path = canvas.static_path(6);
    path.move_to(0.0, 0.0);
    path.curve_to(radius * c, 0.0, radius, radius - radius * c, radius, radius);
    path.line_to(radius, float_height - radius);
    path.curve_to(
        radius,
        float_height - radius + radius * c,
        radius * c,
        float_height,
        0.0,
        float_height,
    );
    path.close();

    canvas.fill_path(&path, FillOptions::default(), Color::ON)
}

/// 
pub fn drawe0b5(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = cp;

    let float_width = width as f64;
    let float_height = height as f64;

    // Coefficient for approximating a circular arc.
    let c: f64 = (std::f64::consts::SQRT_2 - 1.0) * 4.0 / 3.0;

    let radius: f64 = float_width.min(float_height / 2.0);

    // Note on the first/final lineTo moves: this is to enforce horizontal
    // lines on both ends. This ensures that when the path is offset and
    // stroked, the ends are butt-capped entirely perpendicularly; curveTo
    // lerping would produce very slight slopes otherwise.
    let mut path = canvas.static_path(6);
    path.move_to(0.0, 0.0);
    path.line_to(1.0, 0.0);
    path.curve_to(radius * c, 0.0, radius, radius - radius * c, radius, radius);
    path.line_to(radius, float_height - radius);
    path.curve_to(
        radius,
        float_height - radius + radius * c,
        radius * c,
        float_height,
        1.0,
        float_height,
    );
    path.line_to(0.0, float_height);

    canvas.inner_stroke_path(
        &path,
        StrokeOptions {
            line_width: metrics.box_thickness as f64,
            line_cap_mode: CapMode::Butt,
            ..StrokeOptions::default()
        },
        Color::ON,
    )
}

/// 
pub fn drawe0b6(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    drawe0b4(cp, canvas, width, height, metrics)?;
    canvas.flip_horizontal()
}

/// 
pub fn drawe0b7(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    drawe0b5(cp, canvas, width, height, metrics)?;
    canvas.flip_horizontal()
}

/// 
pub fn drawe0d2(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = cp;

    let float_width = width as f64;
    let float_height = height as f64;
    let float_thick = metrics.box_thickness as f64;

    // Top piece
    {
        let mut path = canvas.static_path(6);
        path.move_to(0.0, 0.0);
        path.line_to(float_width, 0.0);
        path.line_to(float_width / 2.0, float_height / 2.0 - float_thick / 2.0);
        path.line_to(0.0, float_height / 2.0 - float_thick / 2.0);
        path.close();

        canvas.fill_path(&path, FillOptions::default(), Color::ON)?;
    }

    // Bottom piece
    {
        let mut path = canvas.static_path(6);
        path.move_to(0.0, float_height);
        path.line_to(float_width, float_height);
        path.line_to(float_width / 2.0, float_height / 2.0 + float_thick / 2.0);
        path.line_to(0.0, float_height / 2.0 + float_thick / 2.0);
        path.close();

        canvas.fill_path(&path, FillOptions::default(), Color::ON)?;
    }
    Ok(())
}

/// 
pub fn drawe0d4(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    drawe0d2(cp, canvas, width, height, metrics)?;
    canvas.flip_horizontal()
}
