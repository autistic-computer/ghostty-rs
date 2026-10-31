//! Port of `src/font/sprite/draw/box.zig`.
//!
//! STATUS: ported
//!
//! Box Drawing | U+2500...U+257F
//! https://en.wikipedia.org/wiki/Box_Drawing
//!
//! ─━│┃┄┅┆┇┈┉┊┋┌┍┎┏
//! ┐┑┒┓└┕┖┗┘┙┚┛├┝┞┟
//! ┠┡┢┣┤┥┦┧┨┩┪┫┬┭┮┯
//! ┰┱┲┳┴┵┶┷┸┹┺┻┼┽┾┿
//! ╀╁╂╃╄╅╆╇╈╉╊╋╌╍╎╏
//! ═║╒╓╔╕╖╗╘╙╚╛╜╝╞╟
//! ╠╡╢╣╤╥╦╧╨╩╪╫╬╭╮╯
//! ╰╱╲╳╴╵╶╷╸╹╺╻╼╽╾╿
//!

use super::common::{hline, hline_middle, vline, vline_middle, Corner, Thickness};
use crate::font::metrics::Metrics;
use crate::font::sprite::canvas::{CapMode, Canvas, Color, Line, Point, StrokeOptions};
use crate::font::sprite::face::DrawFnError;

/// Specification of a traditional intersection-style line/box-drawing char,
/// which can have a different style of line from each edge to the center.
///
/// (Zig `packed struct(u8)` of four 2-bit styles.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lines {
    pub up: Style,
    pub right: Style,
    pub down: Style,
    pub left: Style,
}

impl Lines {
    pub const NONE: Lines = Lines {
        up: Style::None,
        right: Style::None,
        down: Style::None,
        left: Style::None,
    };
}

impl Default for Lines {
    fn default() -> Self {
        Lines::NONE
    }
}

/// Zig `Lines.Style`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Style {
    None,
    Light,
    Heavy,
    Double,
}

#[rustfmt::skip]
pub fn draw2500_257f(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = width;
    let _ = height;

    match cp {
        // '─'
        0x2500 => lines_char(metrics, canvas, Lines { left: Style::Light, right: Style::Light, ..Lines::NONE }),
        // '━'
        0x2501 => lines_char(metrics, canvas, Lines { left: Style::Heavy, right: Style::Heavy, ..Lines::NONE }),
        // '│'
        0x2502 => lines_char(metrics, canvas, Lines { up: Style::Light, down: Style::Light, ..Lines::NONE }),
        // '┃'
        0x2503 => lines_char(metrics, canvas, Lines { up: Style::Heavy, down: Style::Heavy, ..Lines::NONE }),
        // '┄'
        0x2504 => dash_horizontal(
            metrics,
            canvas,
            3,
            Thickness::Light.height(metrics.box_thickness),
            Thickness::Light.height(metrics.box_thickness).max(4),
        ),
        // '┅'
        0x2505 => dash_horizontal(
            metrics,
            canvas,
            3,
            Thickness::Heavy.height(metrics.box_thickness),
            Thickness::Light.height(metrics.box_thickness).max(4),
        ),
        // '┆'
        0x2506 => dash_vertical(
            metrics,
            canvas,
            3,
            Thickness::Light.height(metrics.box_thickness),
            Thickness::Light.height(metrics.box_thickness).max(4),
        ),
        // '┇'
        0x2507 => dash_vertical(
            metrics,
            canvas,
            3,
            Thickness::Heavy.height(metrics.box_thickness),
            Thickness::Light.height(metrics.box_thickness).max(4),
        ),
        // '┈'
        0x2508 => dash_horizontal(
            metrics,
            canvas,
            4,
            Thickness::Light.height(metrics.box_thickness),
            Thickness::Light.height(metrics.box_thickness).max(4),
        ),
        // '┉'
        0x2509 => dash_horizontal(
            metrics,
            canvas,
            4,
            Thickness::Heavy.height(metrics.box_thickness),
            Thickness::Light.height(metrics.box_thickness).max(4),
        ),
        // '┊'
        0x250a => dash_vertical(
            metrics,
            canvas,
            4,
            Thickness::Light.height(metrics.box_thickness),
            Thickness::Light.height(metrics.box_thickness).max(4),
        ),
        // '┋'
        0x250b => dash_vertical(
            metrics,
            canvas,
            4,
            Thickness::Heavy.height(metrics.box_thickness),
            Thickness::Light.height(metrics.box_thickness).max(4),
        ),
        // '┌'
        0x250c => lines_char(metrics, canvas, Lines { down: Style::Light, right: Style::Light, ..Lines::NONE }),
        // '┍'
        0x250d => lines_char(metrics, canvas, Lines { down: Style::Light, right: Style::Heavy, ..Lines::NONE }),
        // '┎'
        0x250e => lines_char(metrics, canvas, Lines { down: Style::Heavy, right: Style::Light, ..Lines::NONE }),
        // '┏'
        0x250f => lines_char(metrics, canvas, Lines { down: Style::Heavy, right: Style::Heavy, ..Lines::NONE }),

        // '┐'
        0x2510 => lines_char(metrics, canvas, Lines { down: Style::Light, left: Style::Light, ..Lines::NONE }),
        // '┑'
        0x2511 => lines_char(metrics, canvas, Lines { down: Style::Light, left: Style::Heavy, ..Lines::NONE }),
        // '┒'
        0x2512 => lines_char(metrics, canvas, Lines { down: Style::Heavy, left: Style::Light, ..Lines::NONE }),
        // '┓'
        0x2513 => lines_char(metrics, canvas, Lines { down: Style::Heavy, left: Style::Heavy, ..Lines::NONE }),
        // '└'
        0x2514 => lines_char(metrics, canvas, Lines { up: Style::Light, right: Style::Light, ..Lines::NONE }),
        // '┕'
        0x2515 => lines_char(metrics, canvas, Lines { up: Style::Light, right: Style::Heavy, ..Lines::NONE }),
        // '┖'
        0x2516 => lines_char(metrics, canvas, Lines { up: Style::Heavy, right: Style::Light, ..Lines::NONE }),
        // '┗'
        0x2517 => lines_char(metrics, canvas, Lines { up: Style::Heavy, right: Style::Heavy, ..Lines::NONE }),
        // '┘'
        0x2518 => lines_char(metrics, canvas, Lines { up: Style::Light, left: Style::Light, ..Lines::NONE }),
        // '┙'
        0x2519 => lines_char(metrics, canvas, Lines { up: Style::Light, left: Style::Heavy, ..Lines::NONE }),
        // '┚'
        0x251a => lines_char(metrics, canvas, Lines { up: Style::Heavy, left: Style::Light, ..Lines::NONE }),
        // '┛'
        0x251b => lines_char(metrics, canvas, Lines { up: Style::Heavy, left: Style::Heavy, ..Lines::NONE }),
        // '├'
        0x251c => lines_char(metrics, canvas, Lines { up: Style::Light, down: Style::Light, right: Style::Light, ..Lines::NONE }),
        // '┝'
        0x251d => lines_char(metrics, canvas, Lines { up: Style::Light, down: Style::Light, right: Style::Heavy, ..Lines::NONE }),
        // '┞'
        0x251e => lines_char(metrics, canvas, Lines { up: Style::Heavy, right: Style::Light, down: Style::Light, ..Lines::NONE }),
        // '┟'
        0x251f => lines_char(metrics, canvas, Lines { down: Style::Heavy, right: Style::Light, up: Style::Light, ..Lines::NONE }),

        // '┠'
        0x2520 => lines_char(metrics, canvas, Lines { up: Style::Heavy, down: Style::Heavy, right: Style::Light, ..Lines::NONE }),
        // '┡'
        0x2521 => lines_char(metrics, canvas, Lines { down: Style::Light, right: Style::Heavy, up: Style::Heavy, ..Lines::NONE }),
        // '┢'
        0x2522 => lines_char(metrics, canvas, Lines { up: Style::Light, right: Style::Heavy, down: Style::Heavy, ..Lines::NONE }),
        // '┣'
        0x2523 => lines_char(metrics, canvas, Lines { up: Style::Heavy, down: Style::Heavy, right: Style::Heavy, ..Lines::NONE }),
        // '┤'
        0x2524 => lines_char(metrics, canvas, Lines { up: Style::Light, down: Style::Light, left: Style::Light, ..Lines::NONE }),
        // '┥'
        0x2525 => lines_char(metrics, canvas, Lines { up: Style::Light, down: Style::Light, left: Style::Heavy, ..Lines::NONE }),
        // '┦'
        0x2526 => lines_char(metrics, canvas, Lines { up: Style::Heavy, left: Style::Light, down: Style::Light, ..Lines::NONE }),
        // '┧'
        0x2527 => lines_char(metrics, canvas, Lines { down: Style::Heavy, left: Style::Light, up: Style::Light, ..Lines::NONE }),
        // '┨'
        0x2528 => lines_char(metrics, canvas, Lines { up: Style::Heavy, down: Style::Heavy, left: Style::Light, ..Lines::NONE }),
        // '┩'
        0x2529 => lines_char(metrics, canvas, Lines { down: Style::Light, left: Style::Heavy, up: Style::Heavy, ..Lines::NONE }),
        // '┪'
        0x252a => lines_char(metrics, canvas, Lines { up: Style::Light, left: Style::Heavy, down: Style::Heavy, ..Lines::NONE }),
        // '┫'
        0x252b => lines_char(metrics, canvas, Lines { up: Style::Heavy, down: Style::Heavy, left: Style::Heavy, ..Lines::NONE }),
        // '┬'
        0x252c => lines_char(metrics, canvas, Lines { down: Style::Light, left: Style::Light, right: Style::Light, ..Lines::NONE }),
        // '┭'
        0x252d => lines_char(metrics, canvas, Lines { left: Style::Heavy, right: Style::Light, down: Style::Light, ..Lines::NONE }),
        // '┮'
        0x252e => lines_char(metrics, canvas, Lines { right: Style::Heavy, left: Style::Light, down: Style::Light, ..Lines::NONE }),
        // '┯'
        0x252f => lines_char(metrics, canvas, Lines { down: Style::Light, left: Style::Heavy, right: Style::Heavy, ..Lines::NONE }),

        // '┰'
        0x2530 => lines_char(metrics, canvas, Lines { down: Style::Heavy, left: Style::Light, right: Style::Light, ..Lines::NONE }),
        // '┱'
        0x2531 => lines_char(metrics, canvas, Lines { right: Style::Light, left: Style::Heavy, down: Style::Heavy, ..Lines::NONE }),
        // '┲'
        0x2532 => lines_char(metrics, canvas, Lines { left: Style::Light, right: Style::Heavy, down: Style::Heavy, ..Lines::NONE }),
        // '┳'
        0x2533 => lines_char(metrics, canvas, Lines { down: Style::Heavy, left: Style::Heavy, right: Style::Heavy, ..Lines::NONE }),
        // '┴'
        0x2534 => lines_char(metrics, canvas, Lines { up: Style::Light, left: Style::Light, right: Style::Light, ..Lines::NONE }),
        // '┵'
        0x2535 => lines_char(metrics, canvas, Lines { left: Style::Heavy, right: Style::Light, up: Style::Light, ..Lines::NONE }),
        // '┶'
        0x2536 => lines_char(metrics, canvas, Lines { right: Style::Heavy, left: Style::Light, up: Style::Light, ..Lines::NONE }),
        // '┷'
        0x2537 => lines_char(metrics, canvas, Lines { up: Style::Light, left: Style::Heavy, right: Style::Heavy, ..Lines::NONE }),
        // '┸'
        0x2538 => lines_char(metrics, canvas, Lines { up: Style::Heavy, left: Style::Light, right: Style::Light, ..Lines::NONE }),
        // '┹'
        0x2539 => lines_char(metrics, canvas, Lines { right: Style::Light, left: Style::Heavy, up: Style::Heavy, ..Lines::NONE }),
        // '┺'
        0x253a => lines_char(metrics, canvas, Lines { left: Style::Light, right: Style::Heavy, up: Style::Heavy, ..Lines::NONE }),
        // '┻'
        0x253b => lines_char(metrics, canvas, Lines { up: Style::Heavy, left: Style::Heavy, right: Style::Heavy, ..Lines::NONE }),
        // '┼'
        0x253c => lines_char(metrics, canvas, Lines { up: Style::Light, down: Style::Light, left: Style::Light, right: Style::Light, ..Lines::NONE }),
        // '┽'
        0x253d => lines_char(metrics, canvas, Lines { left: Style::Heavy, right: Style::Light, up: Style::Light, down: Style::Light, ..Lines::NONE }),
        // '┾'
        0x253e => lines_char(metrics, canvas, Lines { right: Style::Heavy, left: Style::Light, up: Style::Light, down: Style::Light, ..Lines::NONE }),
        // '┿'
        0x253f => lines_char(metrics, canvas, Lines { up: Style::Light, down: Style::Light, left: Style::Heavy, right: Style::Heavy, ..Lines::NONE }),

        // '╀'
        0x2540 => lines_char(metrics, canvas, Lines { up: Style::Heavy, down: Style::Light, left: Style::Light, right: Style::Light, ..Lines::NONE }),
        // '╁'
        0x2541 => lines_char(metrics, canvas, Lines { down: Style::Heavy, up: Style::Light, left: Style::Light, right: Style::Light, ..Lines::NONE }),
        // '╂'
        0x2542 => lines_char(metrics, canvas, Lines { up: Style::Heavy, down: Style::Heavy, left: Style::Light, right: Style::Light, ..Lines::NONE }),
        // '╃'
        0x2543 => lines_char(metrics, canvas, Lines { left: Style::Heavy, up: Style::Heavy, right: Style::Light, down: Style::Light, ..Lines::NONE }),
        // '╄'
        0x2544 => lines_char(metrics, canvas, Lines { right: Style::Heavy, up: Style::Heavy, left: Style::Light, down: Style::Light, ..Lines::NONE }),
        // '╅'
        0x2545 => lines_char(metrics, canvas, Lines { left: Style::Heavy, down: Style::Heavy, right: Style::Light, up: Style::Light, ..Lines::NONE }),
        // '╆'
        0x2546 => lines_char(metrics, canvas, Lines { right: Style::Heavy, down: Style::Heavy, left: Style::Light, up: Style::Light, ..Lines::NONE }),
        // '╇'
        0x2547 => lines_char(metrics, canvas, Lines { down: Style::Light, up: Style::Heavy, left: Style::Heavy, right: Style::Heavy, ..Lines::NONE }),
        // '╈'
        0x2548 => lines_char(metrics, canvas, Lines { up: Style::Light, down: Style::Heavy, left: Style::Heavy, right: Style::Heavy, ..Lines::NONE }),
        // '╉'
        0x2549 => lines_char(metrics, canvas, Lines { right: Style::Light, left: Style::Heavy, up: Style::Heavy, down: Style::Heavy, ..Lines::NONE }),
        // '╊'
        0x254a => lines_char(metrics, canvas, Lines { left: Style::Light, right: Style::Heavy, up: Style::Heavy, down: Style::Heavy, ..Lines::NONE }),
        // '╋'
        0x254b => lines_char(metrics, canvas, Lines { up: Style::Heavy, down: Style::Heavy, left: Style::Heavy, right: Style::Heavy, ..Lines::NONE }),
        // '╌'
        0x254c => dash_horizontal(
            metrics,
            canvas,
            2,
            Thickness::Light.height(metrics.box_thickness),
            Thickness::Light.height(metrics.box_thickness),
        ),
        // '╍'
        0x254d => dash_horizontal(
            metrics,
            canvas,
            2,
            Thickness::Heavy.height(metrics.box_thickness),
            Thickness::Heavy.height(metrics.box_thickness),
        ),
        // '╎'
        0x254e => dash_vertical(
            metrics,
            canvas,
            2,
            Thickness::Light.height(metrics.box_thickness),
            Thickness::Heavy.height(metrics.box_thickness),
        ),
        // '╏'
        0x254f => dash_vertical(
            metrics,
            canvas,
            2,
            Thickness::Heavy.height(metrics.box_thickness),
            Thickness::Heavy.height(metrics.box_thickness),
        ),

        // '═'
        0x2550 => lines_char(metrics, canvas, Lines { left: Style::Double, right: Style::Double, ..Lines::NONE }),
        // '║'
        0x2551 => lines_char(metrics, canvas, Lines { up: Style::Double, down: Style::Double, ..Lines::NONE }),
        // '╒'
        0x2552 => lines_char(metrics, canvas, Lines { down: Style::Light, right: Style::Double, ..Lines::NONE }),
        // '╓'
        0x2553 => lines_char(metrics, canvas, Lines { down: Style::Double, right: Style::Light, ..Lines::NONE }),
        // '╔'
        0x2554 => lines_char(metrics, canvas, Lines { down: Style::Double, right: Style::Double, ..Lines::NONE }),
        // '╕'
        0x2555 => lines_char(metrics, canvas, Lines { down: Style::Light, left: Style::Double, ..Lines::NONE }),
        // '╖'
        0x2556 => lines_char(metrics, canvas, Lines { down: Style::Double, left: Style::Light, ..Lines::NONE }),
        // '╗'
        0x2557 => lines_char(metrics, canvas, Lines { down: Style::Double, left: Style::Double, ..Lines::NONE }),
        // '╘'
        0x2558 => lines_char(metrics, canvas, Lines { up: Style::Light, right: Style::Double, ..Lines::NONE }),
        // '╙'
        0x2559 => lines_char(metrics, canvas, Lines { up: Style::Double, right: Style::Light, ..Lines::NONE }),
        // '╚'
        0x255a => lines_char(metrics, canvas, Lines { up: Style::Double, right: Style::Double, ..Lines::NONE }),
        // '╛'
        0x255b => lines_char(metrics, canvas, Lines { up: Style::Light, left: Style::Double, ..Lines::NONE }),
        // '╜'
        0x255c => lines_char(metrics, canvas, Lines { up: Style::Double, left: Style::Light, ..Lines::NONE }),
        // '╝'
        0x255d => lines_char(metrics, canvas, Lines { up: Style::Double, left: Style::Double, ..Lines::NONE }),
        // '╞'
        0x255e => lines_char(metrics, canvas, Lines { up: Style::Light, down: Style::Light, right: Style::Double, ..Lines::NONE }),
        // '╟'
        0x255f => lines_char(metrics, canvas, Lines { up: Style::Double, down: Style::Double, right: Style::Light, ..Lines::NONE }),

        // '╠'
        0x2560 => lines_char(metrics, canvas, Lines { up: Style::Double, down: Style::Double, right: Style::Double, ..Lines::NONE }),
        // '╡'
        0x2561 => lines_char(metrics, canvas, Lines { up: Style::Light, down: Style::Light, left: Style::Double, ..Lines::NONE }),
        // '╢'
        0x2562 => lines_char(metrics, canvas, Lines { up: Style::Double, down: Style::Double, left: Style::Light, ..Lines::NONE }),
        // '╣'
        0x2563 => lines_char(metrics, canvas, Lines { up: Style::Double, down: Style::Double, left: Style::Double, ..Lines::NONE }),
        // '╤'
        0x2564 => lines_char(metrics, canvas, Lines { down: Style::Light, left: Style::Double, right: Style::Double, ..Lines::NONE }),
        // '╥'
        0x2565 => lines_char(metrics, canvas, Lines { down: Style::Double, left: Style::Light, right: Style::Light, ..Lines::NONE }),
        // '╦'
        0x2566 => lines_char(metrics, canvas, Lines { down: Style::Double, left: Style::Double, right: Style::Double, ..Lines::NONE }),
        // '╧'
        0x2567 => lines_char(metrics, canvas, Lines { up: Style::Light, left: Style::Double, right: Style::Double, ..Lines::NONE }),
        // '╨'
        0x2568 => lines_char(metrics, canvas, Lines { up: Style::Double, left: Style::Light, right: Style::Light, ..Lines::NONE }),
        // '╩'
        0x2569 => lines_char(metrics, canvas, Lines { up: Style::Double, left: Style::Double, right: Style::Double, ..Lines::NONE }),
        // '╪'
        0x256a => lines_char(metrics, canvas, Lines { up: Style::Light, down: Style::Light, left: Style::Double, right: Style::Double, ..Lines::NONE }),
        // '╫'
        0x256b => lines_char(metrics, canvas, Lines { up: Style::Double, down: Style::Double, left: Style::Light, right: Style::Light, ..Lines::NONE }),
        // '╬'
        0x256c => lines_char(metrics, canvas, Lines { up: Style::Double, down: Style::Double, left: Style::Double, right: Style::Double, ..Lines::NONE }),
        // '╭'
        0x256d => arc(metrics, canvas, Corner::Br, Thickness::Light)?,
        // '╮'
        0x256e => arc(metrics, canvas, Corner::Bl, Thickness::Light)?,
        // '╯'
        0x256f => arc(metrics, canvas, Corner::Tl, Thickness::Light)?,

        // '╰'
        0x2570 => arc(metrics, canvas, Corner::Tr, Thickness::Light)?,
        // '╱'
        0x2571 => light_diagonal_upper_right_to_lower_left(metrics, canvas),
        // '╲'
        0x2572 => light_diagonal_upper_left_to_lower_right(metrics, canvas),
        // '╳'
        0x2573 => light_diagonal_cross(metrics, canvas),
        // '╴'
        0x2574 => lines_char(metrics, canvas, Lines { left: Style::Light, ..Lines::NONE }),
        // '╵'
        0x2575 => lines_char(metrics, canvas, Lines { up: Style::Light, ..Lines::NONE }),
        // '╶'
        0x2576 => lines_char(metrics, canvas, Lines { right: Style::Light, ..Lines::NONE }),
        // '╷'
        0x2577 => lines_char(metrics, canvas, Lines { down: Style::Light, ..Lines::NONE }),
        // '╸'
        0x2578 => lines_char(metrics, canvas, Lines { left: Style::Heavy, ..Lines::NONE }),
        // '╹'
        0x2579 => lines_char(metrics, canvas, Lines { up: Style::Heavy, ..Lines::NONE }),
        // '╺'
        0x257a => lines_char(metrics, canvas, Lines { right: Style::Heavy, ..Lines::NONE }),
        // '╻'
        0x257b => lines_char(metrics, canvas, Lines { down: Style::Heavy, ..Lines::NONE }),
        // '╼'
        0x257c => lines_char(metrics, canvas, Lines { left: Style::Light, right: Style::Heavy, ..Lines::NONE }),
        // '╽'
        0x257d => lines_char(metrics, canvas, Lines { up: Style::Light, down: Style::Heavy, ..Lines::NONE }),
        // '╾'
        0x257e => lines_char(metrics, canvas, Lines { left: Style::Heavy, right: Style::Light, ..Lines::NONE }),
        // '╿'
        0x257f => lines_char(metrics, canvas, Lines { up: Style::Heavy, down: Style::Light, ..Lines::NONE }),

        _ => unreachable!(),
    }
    Ok(())
}

pub fn lines_char(metrics: Metrics, canvas: &mut Canvas, lines: Lines) {
    let light_px = Thickness::Light.height(metrics.box_thickness);
    let heavy_px = Thickness::Heavy.height(metrics.box_thickness);

    // Top of light horizontal strokes
    let h_light_top = metrics.cell_height.saturating_sub(light_px) / 2;
    // Bottom of light horizontal strokes
    let h_light_bottom = h_light_top.saturating_add(light_px);

    // Top of heavy horizontal strokes
    let h_heavy_top = metrics.cell_height.saturating_sub(heavy_px) / 2;
    // Bottom of heavy horizontal strokes
    let h_heavy_bottom = h_heavy_top.saturating_add(heavy_px);

    // Top of the top doubled horizontal stroke (bottom is `h_light_top`)
    let h_double_top = h_light_top.saturating_sub(light_px);
    // Bottom of the bottom doubled horizontal stroke (top is `h_light_bottom`)
    let h_double_bottom = h_light_bottom.saturating_add(light_px);

    // Left of light vertical strokes
    let v_light_left = metrics.cell_width.saturating_sub(light_px) / 2;
    // Right of light vertical strokes
    let v_light_right = v_light_left.saturating_add(light_px);

    // Left of heavy vertical strokes
    let v_heavy_left = metrics.cell_width.saturating_sub(heavy_px) / 2;
    // Right of heavy vertical strokes
    let v_heavy_right = v_heavy_left.saturating_add(heavy_px);

    // Left of the left doubled vertical stroke (right is `v_light_left`)
    let v_double_left = v_light_left.saturating_sub(light_px);
    // Right of the right doubled vertical stroke (left is `v_light_right`)
    let v_double_right = v_light_right.saturating_add(light_px);

    // The bottom of the up line
    let up_bottom = if lines.left == Style::Heavy || lines.right == Style::Heavy {
        h_heavy_bottom
    } else if lines.left != lines.right || lines.down == lines.up {
        if lines.left == Style::Double || lines.right == Style::Double {
            h_double_bottom
        } else {
            h_light_bottom
        }
    } else if lines.left == Style::None && lines.right == Style::None {
        h_light_bottom
    } else {
        h_light_top
    };

    // The top of the down line
    let down_top = if lines.left == Style::Heavy || lines.right == Style::Heavy {
        h_heavy_top
    } else if lines.left != lines.right || lines.up == lines.down {
        if lines.left == Style::Double || lines.right == Style::Double {
            h_double_top
        } else {
            h_light_top
        }
    } else if lines.left == Style::None && lines.right == Style::None {
        h_light_top
    } else {
        h_light_bottom
    };

    // The right of the left line
    let left_right = if lines.up == Style::Heavy || lines.down == Style::Heavy {
        v_heavy_right
    } else if lines.up != lines.down || lines.left == lines.right {
        if lines.up == Style::Double || lines.down == Style::Double {
            v_double_right
        } else {
            v_light_right
        }
    } else if lines.up == Style::None && lines.down == Style::None {
        v_light_right
    } else {
        v_light_left
    };

    // The left of the right line
    let right_left = if lines.up == Style::Heavy || lines.down == Style::Heavy {
        v_heavy_left
    } else if lines.up != lines.down || lines.right == lines.left {
        if lines.up == Style::Double || lines.down == Style::Double {
            v_double_left
        } else {
            v_light_left
        }
    } else if lines.up == Style::None && lines.down == Style::None {
        v_light_left
    } else {
        v_light_right
    };

    let cell_width = metrics.cell_width as i32;
    let cell_height = metrics.cell_height as i32;

    match lines.up {
        Style::None => {}
        Style::Light => canvas.r#box(
            v_light_left as i32,
            0,
            v_light_right as i32,
            up_bottom as i32,
            Color::ON,
        ),
        Style::Heavy => canvas.r#box(
            v_heavy_left as i32,
            0,
            v_heavy_right as i32,
            up_bottom as i32,
            Color::ON,
        ),
        Style::Double => {
            let left_bottom = if lines.left == Style::Double {
                h_light_top
            } else {
                up_bottom
            };
            let right_bottom = if lines.right == Style::Double {
                h_light_top
            } else {
                up_bottom
            };

            canvas.r#box(
                v_double_left as i32,
                0,
                v_light_left as i32,
                left_bottom as i32,
                Color::ON,
            );
            canvas.r#box(
                v_light_right as i32,
                0,
                v_double_right as i32,
                right_bottom as i32,
                Color::ON,
            );
        }
    }

    match lines.right {
        Style::None => {}
        Style::Light => canvas.r#box(
            right_left as i32,
            h_light_top as i32,
            cell_width,
            h_light_bottom as i32,
            Color::ON,
        ),
        Style::Heavy => canvas.r#box(
            right_left as i32,
            h_heavy_top as i32,
            cell_width,
            h_heavy_bottom as i32,
            Color::ON,
        ),
        Style::Double => {
            let top_left = if lines.up == Style::Double {
                v_light_right
            } else {
                right_left
            };
            let bottom_left = if lines.down == Style::Double {
                v_light_right
            } else {
                right_left
            };

            canvas.r#box(
                top_left as i32,
                h_double_top as i32,
                cell_width,
                h_light_top as i32,
                Color::ON,
            );
            canvas.r#box(
                bottom_left as i32,
                h_light_bottom as i32,
                cell_width,
                h_double_bottom as i32,
                Color::ON,
            );
        }
    }

    match lines.down {
        Style::None => {}
        Style::Light => canvas.r#box(
            v_light_left as i32,
            down_top as i32,
            v_light_right as i32,
            cell_height,
            Color::ON,
        ),
        Style::Heavy => canvas.r#box(
            v_heavy_left as i32,
            down_top as i32,
            v_heavy_right as i32,
            cell_height,
            Color::ON,
        ),
        Style::Double => {
            let left_top = if lines.left == Style::Double {
                h_light_bottom
            } else {
                down_top
            };
            let right_top = if lines.right == Style::Double {
                h_light_bottom
            } else {
                down_top
            };

            canvas.r#box(
                v_double_left as i32,
                left_top as i32,
                v_light_left as i32,
                cell_height,
                Color::ON,
            );
            canvas.r#box(
                v_light_right as i32,
                right_top as i32,
                v_double_right as i32,
                cell_height,
                Color::ON,
            );
        }
    }

    match lines.left {
        Style::None => {}
        Style::Light => canvas.r#box(
            0,
            h_light_top as i32,
            left_right as i32,
            h_light_bottom as i32,
            Color::ON,
        ),
        Style::Heavy => canvas.r#box(
            0,
            h_heavy_top as i32,
            left_right as i32,
            h_heavy_bottom as i32,
            Color::ON,
        ),
        Style::Double => {
            let top_right = if lines.up == Style::Double {
                v_light_left
            } else {
                left_right
            };
            let bottom_right = if lines.down == Style::Double {
                v_light_left
            } else {
                left_right
            };

            canvas.r#box(
                0,
                h_double_top as i32,
                top_right as i32,
                h_light_top as i32,
                Color::ON,
            );
            canvas.r#box(
                0,
                h_light_bottom as i32,
                bottom_right as i32,
                h_double_bottom as i32,
                Color::ON,
            );
        }
    }
}

pub fn light_diagonal_upper_right_to_lower_left(metrics: Metrics, canvas: &mut Canvas) {
    let float_width = metrics.cell_width as f64;
    let float_height = metrics.cell_height as f64;

    // We overshoot the corners by a tiny bit, but we need to
    // maintain the correct slope, so we calculate that here.
    let slope_x: f64 = (float_width / float_height).min(1.0);
    let slope_y: f64 = (float_height / float_width).min(1.0);

    let _ = canvas.line(
        Line {
            p0: Point {
                x: float_width + 0.5 * slope_x,
                y: -0.5 * slope_y,
            },
            p1: Point {
                x: -0.5 * slope_x,
                y: float_height + 0.5 * slope_y,
            },
        },
        Thickness::Light.height(metrics.box_thickness) as f64,
        Color::ON,
    );
}

pub fn light_diagonal_upper_left_to_lower_right(metrics: Metrics, canvas: &mut Canvas) {
    let float_width = metrics.cell_width as f64;
    let float_height = metrics.cell_height as f64;

    // We overshoot the corners by a tiny bit, but we need to
    // maintain the correct slope, so we calculate that here.
    let slope_x: f64 = (float_width / float_height).min(1.0);
    let slope_y: f64 = (float_height / float_width).min(1.0);

    let _ = canvas.line(
        Line {
            p0: Point {
                x: -0.5 * slope_x,
                y: -0.5 * slope_y,
            },
            p1: Point {
                x: float_width + 0.5 * slope_x,
                y: float_height + 0.5 * slope_y,
            },
        },
        Thickness::Light.height(metrics.box_thickness) as f64,
        Color::ON,
    );
}

pub fn light_diagonal_cross(metrics: Metrics, canvas: &mut Canvas) {
    light_diagonal_upper_right_to_lower_left(metrics, canvas);
    light_diagonal_upper_left_to_lower_right(metrics, canvas);
}

pub fn arc(
    metrics: Metrics,
    canvas: &mut Canvas,
    corner: Corner,
    thickness: Thickness,
) -> Result<(), DrawFnError> {
    let thick_px = thickness.height(metrics.box_thickness);
    let float_width = metrics.cell_width as f64;
    let float_height = metrics.cell_height as f64;
    let float_thick = thick_px as f64;
    let center_x: f64 =
        (metrics.cell_width.saturating_sub(thick_px) / 2) as f64 + float_thick / 2.0;
    let center_y: f64 =
        (metrics.cell_height.saturating_sub(thick_px) / 2) as f64 + float_thick / 2.0;

    let r = float_width.min(float_height) / 2.0;

    // Fraction away from the center to place the middle control points,
    let s: f64 = 0.25;

    let mut path = canvas.static_path(4);

    match corner {
        Corner::Tl => {
            path.move_to(center_x, 0.0);
            path.line_to(center_x, center_y - r);
            path.curve_to(
                center_x,
                center_y - s * r,
                center_x - s * r,
                center_y,
                center_x - r,
                center_y,
            );
            path.line_to(0.0, center_y);
        }
        Corner::Tr => {
            path.move_to(center_x, 0.0);
            path.line_to(center_x, center_y - r);
            path.curve_to(
                center_x,
                center_y - s * r,
                center_x + s * r,
                center_y,
                center_x + r,
                center_y,
            );
            path.line_to(float_width, center_y);
        }
        Corner::Bl => {
            path.move_to(center_x, float_height);
            path.line_to(center_x, center_y + r);
            path.curve_to(
                center_x,
                center_y + s * r,
                center_x - s * r,
                center_y,
                center_x - r,
                center_y,
            );
            path.line_to(0.0, center_y);
        }
        Corner::Br => {
            path.move_to(center_x, float_height);
            path.line_to(center_x, center_y + r);
            path.curve_to(
                center_x,
                center_y + s * r,
                center_x + s * r,
                center_y,
                center_x + r,
                center_y,
            );
            path.line_to(float_width, center_y);
        }
    }

    canvas.stroke_path(
        &path,
        StrokeOptions {
            line_cap_mode: CapMode::Butt,
            line_width: float_thick,
            ..StrokeOptions::default()
        },
        Color::ON,
    )
}

fn dash_horizontal(
    metrics: Metrics,
    canvas: &mut Canvas,
    count: u8,
    thick_px: u32,
    desired_gap: u32,
) {
    debug_assert!((2..=4).contains(&count));

    // +------------+
    // |            |
    // |            |
    // |            |
    // |            |
    // | --  --  -- |
    // |            |
    // |            |
    // |            |
    // |            |
    // +------------+
    // Our dashed line should be made such that when tiled horizontally
    // it creates one consistent line with no uneven gap or segment sizes.
    // In order to make sure this is the case, we should have half-sized
    // gaps on the left and right so that it is centered properly.

    // For N dashes, there are N - 1 gaps between them, but we also have
    // half-sized gaps on either side, adding up to N total gaps.
    let gap_count = count as i32;
    let count_i = count as i32;

    // We need at least 1 pixel for each gap and each dash, if we don't
    // have that then we can't draw our dashed line correctly so we just
    // draw a solid line and return.
    if (metrics.cell_width as i64) < (count_i + gap_count) as i64 {
        hline_middle(metrics, canvas, Thickness::Light);
        return;
    }

    // We never want the gaps to take up more than 50% of the space,
    // because if they do the dashes are too small and look wrong.
    let gap_width: i32 = desired_gap.min(metrics.cell_width / (2 * count as u32)) as i32;
    let total_gap_width: i32 = gap_count * gap_width;
    let total_dash_width: i32 = metrics.cell_width as i32 - total_gap_width;
    let dash_width: i32 = total_dash_width.div_euclid(count_i);
    let remaining: i32 = total_dash_width.rem_euclid(count_i);

    debug_assert!(
        dash_width * count_i + gap_width * gap_count + remaining == metrics.cell_width as i32
    );

    // Our dashes should be centered vertically.
    let y: i32 = (metrics.cell_height.saturating_sub(thick_px) / 2) as i32;

    // We start at half a gap from the left edge, in order to center
    // our dashes properly.
    let mut x: i32 = gap_width.div_euclid(2);

    // We'll distribute the extra space in to dash widths, 1px at a
    // time. We prefer this to making gaps larger since that is much
    // more visually obvious.
    let mut extra: i32 = remaining;

    for _ in 0..count {
        let mut x1 = x + dash_width;
        // We distribute left-over size in to dash widths,
        // since it's less obvious there than in the gaps.
        if extra > 0 {
            extra -= 1;
            x1 += 1;
        }
        hline(canvas, x, x1, y, thick_px);
        // Advance by the width of the dash we drew and the width
        // of a gap to get the start of the next dash.
        x = x1 + gap_width;
    }
}

fn dash_vertical(metrics: Metrics, canvas: &mut Canvas, count: u8, thick_px: u32, desired_gap: u32) {
    debug_assert!((2..=4).contains(&count));

    // +-----------+
    // |     |     |
    // |     |     |
    // |           |
    // |     |     |
    // |     |     |
    // |           |
    // |     |     |
    // |     |     |
    // |           |
    // +-----------+
    // Our dashed line should be made such that when tiled vertically it
    // it creates one consistent line with no uneven gap or segment sizes.
    // In order to make sure this is the case, we should have an extra gap
    // gap at the bottom.
    //
    // A single full-sized extra gap is preferred to two half-sized ones for
    // vertical to allow better joining to solid characters without creating
    // visible half-sized gaps. Unlike horizontal, centering is a lot less
    // important, visually.

    // Because of the extra gap at the bottom, there are as many gaps as
    // there are dashes.
    let gap_count = count as i32;
    let count_i = count as i32;

    // We need at least 1 pixel for each gap and each dash, if we don't
    // have that then we can't draw our dashed line correctly so we just
    // draw a solid line and return.
    if (metrics.cell_height as i64) < (count_i + gap_count) as i64 {
        vline_middle(metrics, canvas, Thickness::Light);
        return;
    }

    // We never want the gaps to take up more than 50% of the space,
    // because if they do the dashes are too small and look wrong.
    let gap_height: i32 = desired_gap.min(metrics.cell_height / (2 * count as u32)) as i32;
    let total_gap_height: i32 = gap_count * gap_height;
    let total_dash_height: i32 = metrics.cell_height as i32 - total_gap_height;
    let dash_height: i32 = total_dash_height.div_euclid(count_i);
    let remaining: i32 = total_dash_height.rem_euclid(count_i);

    debug_assert!(
        dash_height * count_i + gap_height * gap_count + remaining == metrics.cell_height as i32
    );

    // Our dashes should be centered horizontally.
    let x: i32 = (metrics.cell_width.saturating_sub(thick_px) / 2) as i32;

    // We start at the top of the cell.
    let mut y: i32 = 0;

    // We'll distribute the extra space in to dash heights, 1px at a
    // time. We prefer this to making gaps larger since that is much
    // more visually obvious.
    let mut extra: i32 = remaining;

    for _ in 0..count {
        let mut y1 = y + dash_height;
        // We distribute left-over size in to dash widths,
        // since it's less obvious there than in the gaps.
        if extra > 0 {
            extra -= 1;
            y1 += 1;
        }
        vline(canvas, y, y1, x, thick_px);
        // Advance by the height of the dash we drew and the height
        // of a gap to get the start of the next dash.
        y = y1 + gap_height;
    }
}
