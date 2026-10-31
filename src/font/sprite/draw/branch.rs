//! Port of `src/font/sprite/draw/branch.zig`.
//!
//! STATUS: ported
//!
//! Branch Drawing Characters | U+F5D0...U+F60D
//!
//! Branch drawing character set, used for drawing git-like
//! graphs in the terminal. Originally implemented in Kitty.
//! Ref:
//! - https://github.com/kovidgoyal/kitty/pull/7681
//! - https://github.com/kovidgoyal/kitty/pull/7805
//! NOTE: Kitty is GPL licensed, and its code was not referenced
//!       for these characters, only the loose specification of
//!       the character set in the pull request descriptions.
//!
//!                
//!                
//!                
//!              
//!

use super::common::{hline_middle, vline_middle, Corner, Edge, Shade, Thickness};
use crate::font::metrics::Metrics;
use crate::font::sprite::canvas::{Canvas, Color};
use crate::font::sprite::draw::r#box::arc;
use crate::font::sprite::face::DrawFnError;

/// Specification of a branch drawing node, which consists of a
/// circle which is either empty or filled, and lines connecting
/// optionally between the circle and each of the 4 edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct BranchNode {
    up: bool,
    right: bool,
    down: bool,
    left: bool,
    filled: bool,
}

#[rustfmt::skip]
pub fn drawf5d0_f60d(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = width;
    let _ = height;

    match cp {
        // ''
        0x0f5d0 => hline_middle(metrics, canvas, Thickness::Light),
        // ''
        0x0f5d1 => vline_middle(metrics, canvas, Thickness::Light),
        // ''
        0x0f5d2 => fading_line(metrics, canvas, Edge::Right, Thickness::Light),
        // ''
        0x0f5d3 => fading_line(metrics, canvas, Edge::Left, Thickness::Light),
        // ''
        0x0f5d4 => fading_line(metrics, canvas, Edge::Bottom, Thickness::Light),
        // ''
        0x0f5d5 => fading_line(metrics, canvas, Edge::Top, Thickness::Light),
        // ''
        0x0f5d6 => arc(metrics, canvas, Corner::Br, Thickness::Light)?,
        // ''
        0x0f5d7 => arc(metrics, canvas, Corner::Bl, Thickness::Light)?,
        // ''
        0x0f5d8 => arc(metrics, canvas, Corner::Tr, Thickness::Light)?,
        // ''
        0x0f5d9 => arc(metrics, canvas, Corner::Tl, Thickness::Light)?,
        // ''
        0x0f5da => {
            vline_middle(metrics, canvas, Thickness::Light);
            arc(metrics, canvas, Corner::Tr, Thickness::Light)?;
        },
        // ''
        0x0f5db => {
            vline_middle(metrics, canvas, Thickness::Light);
            arc(metrics, canvas, Corner::Br, Thickness::Light)?;
        },
        // ''
        0x0f5dc => {
            arc(metrics, canvas, Corner::Tr, Thickness::Light)?;
            arc(metrics, canvas, Corner::Br, Thickness::Light)?;
        },
        // ''
        0x0f5dd => {
            vline_middle(metrics, canvas, Thickness::Light);
            arc(metrics, canvas, Corner::Tl, Thickness::Light)?;
        },
        // ''
        0x0f5de => {
            vline_middle(metrics, canvas, Thickness::Light);
            arc(metrics, canvas, Corner::Bl, Thickness::Light)?;
        },
        // ''
        0x0f5df => {
            arc(metrics, canvas, Corner::Tl, Thickness::Light)?;
            arc(metrics, canvas, Corner::Bl, Thickness::Light)?;
        },

        // ''
        0x0f5e0 => {
            arc(metrics, canvas, Corner::Bl, Thickness::Light)?;
            hline_middle(metrics, canvas, Thickness::Light);
        },
        // ''
        0x0f5e1 => {
            arc(metrics, canvas, Corner::Br, Thickness::Light)?;
            hline_middle(metrics, canvas, Thickness::Light);
        },
        // ''
        0x0f5e2 => {
            arc(metrics, canvas, Corner::Br, Thickness::Light)?;
            arc(metrics, canvas, Corner::Bl, Thickness::Light)?;
        },
        // ''
        0x0f5e3 => {
            arc(metrics, canvas, Corner::Tl, Thickness::Light)?;
            hline_middle(metrics, canvas, Thickness::Light);
        },
        // ''
        0x0f5e4 => {
            arc(metrics, canvas, Corner::Tr, Thickness::Light)?;
            hline_middle(metrics, canvas, Thickness::Light);
        },
        // ''
        0x0f5e5 => {
            arc(metrics, canvas, Corner::Tr, Thickness::Light)?;
            arc(metrics, canvas, Corner::Tl, Thickness::Light)?;
        },
        // ''
        0x0f5e6 => {
            vline_middle(metrics, canvas, Thickness::Light);
            arc(metrics, canvas, Corner::Tl, Thickness::Light)?;
            arc(metrics, canvas, Corner::Tr, Thickness::Light)?;
        },
        // ''
        0x0f5e7 => {
            vline_middle(metrics, canvas, Thickness::Light);
            arc(metrics, canvas, Corner::Bl, Thickness::Light)?;
            arc(metrics, canvas, Corner::Br, Thickness::Light)?;
        },
        // ''
        0x0f5e8 => {
            hline_middle(metrics, canvas, Thickness::Light);
            arc(metrics, canvas, Corner::Bl, Thickness::Light)?;
            arc(metrics, canvas, Corner::Tl, Thickness::Light)?;
        },
        // ''
        0x0f5e9 => {
            hline_middle(metrics, canvas, Thickness::Light);
            arc(metrics, canvas, Corner::Tr, Thickness::Light)?;
            arc(metrics, canvas, Corner::Br, Thickness::Light)?;
        },
        // ''
        0x0f5ea => {
            vline_middle(metrics, canvas, Thickness::Light);
            arc(metrics, canvas, Corner::Tl, Thickness::Light)?;
            arc(metrics, canvas, Corner::Br, Thickness::Light)?;
        },
        // ''
        0x0f5eb => {
            vline_middle(metrics, canvas, Thickness::Light);
            arc(metrics, canvas, Corner::Tr, Thickness::Light)?;
            arc(metrics, canvas, Corner::Bl, Thickness::Light)?;
        },
        // ''
        0x0f5ec => {
            hline_middle(metrics, canvas, Thickness::Light);
            arc(metrics, canvas, Corner::Tl, Thickness::Light)?;
            arc(metrics, canvas, Corner::Br, Thickness::Light)?;
        },
        // ''
        0x0f5ed => {
            hline_middle(metrics, canvas, Thickness::Light);
            arc(metrics, canvas, Corner::Tr, Thickness::Light)?;
            arc(metrics, canvas, Corner::Bl, Thickness::Light)?;
        },
        // ''
        0x0f5ee => branch_node(metrics, canvas, BranchNode { filled: true, ..Default::default() }, Thickness::Light),
        // ''
        0x0f5ef => branch_node(metrics, canvas, BranchNode::default(), Thickness::Light),

        // ''
        0x0f5f0 => branch_node(metrics, canvas, BranchNode { right: true, filled: true, ..Default::default() }, Thickness::Light),
        // ''
        0x0f5f1 => branch_node(metrics, canvas, BranchNode { right: true, ..Default::default() }, Thickness::Light),
        // ''
        0x0f5f2 => branch_node(metrics, canvas, BranchNode { left: true, filled: true, ..Default::default() }, Thickness::Light),
        // ''
        0x0f5f3 => branch_node(metrics, canvas, BranchNode { left: true, ..Default::default() }, Thickness::Light),
        // ''
        0x0f5f4 => branch_node(metrics, canvas, BranchNode { left: true, right: true, filled: true, ..Default::default() }, Thickness::Light),
        // ''
        0x0f5f5 => branch_node(metrics, canvas, BranchNode { left: true, right: true, ..Default::default() }, Thickness::Light),
        // ''
        0x0f5f6 => branch_node(metrics, canvas, BranchNode { down: true, filled: true, ..Default::default() }, Thickness::Light),
        // ''
        0x0f5f7 => branch_node(metrics, canvas, BranchNode { down: true, ..Default::default() }, Thickness::Light),
        // ''
        0x0f5f8 => branch_node(metrics, canvas, BranchNode { up: true, filled: true, ..Default::default() }, Thickness::Light),
        // ''
        0x0f5f9 => branch_node(metrics, canvas, BranchNode { up: true, ..Default::default() }, Thickness::Light),
        // ''
        0x0f5fa => branch_node(metrics, canvas, BranchNode { up: true, down: true, filled: true, ..Default::default() }, Thickness::Light),
        // ''
        0x0f5fb => branch_node(metrics, canvas, BranchNode { up: true, down: true, ..Default::default() }, Thickness::Light),
        // ''
        0x0f5fc => branch_node(metrics, canvas, BranchNode { right: true, down: true, filled: true, ..Default::default() }, Thickness::Light),
        // ''
        0x0f5fd => branch_node(metrics, canvas, BranchNode { right: true, down: true, ..Default::default() }, Thickness::Light),
        // ''
        0x0f5fe => branch_node(metrics, canvas, BranchNode { left: true, down: true, filled: true, ..Default::default() }, Thickness::Light),
        // ''
        0x0f5ff => branch_node(metrics, canvas, BranchNode { left: true, down: true, ..Default::default() }, Thickness::Light),

        // ''
        0x0f600 => branch_node(metrics, canvas, BranchNode { up: true, right: true, filled: true, ..Default::default() }, Thickness::Light),
        // ''
        0x0f601 => branch_node(metrics, canvas, BranchNode { up: true, right: true, ..Default::default() }, Thickness::Light),
        // ''
        0x0f602 => branch_node(metrics, canvas, BranchNode { up: true, left: true, filled: true, ..Default::default() }, Thickness::Light),
        // ''
        0x0f603 => branch_node(metrics, canvas, BranchNode { up: true, left: true, ..Default::default() }, Thickness::Light),
        // ''
        0x0f604 => branch_node(metrics, canvas, BranchNode { up: true, down: true, right: true, filled: true, ..Default::default() }, Thickness::Light),
        // ''
        0x0f605 => branch_node(metrics, canvas, BranchNode { up: true, down: true, right: true, ..Default::default() }, Thickness::Light),
        // ''
        0x0f606 => branch_node(metrics, canvas, BranchNode { up: true, down: true, left: true, filled: true, ..Default::default() }, Thickness::Light),
        // ''
        0x0f607 => branch_node(metrics, canvas, BranchNode { up: true, down: true, left: true, ..Default::default() }, Thickness::Light),
        // ''
        0x0f608 => branch_node(metrics, canvas, BranchNode { down: true, left: true, right: true, filled: true, ..Default::default() }, Thickness::Light),
        // ''
        0x0f609 => branch_node(metrics, canvas, BranchNode { down: true, left: true, right: true, ..Default::default() }, Thickness::Light),
        // ''
        0x0f60a => branch_node(metrics, canvas, BranchNode { up: true, left: true, right: true, filled: true, ..Default::default() }, Thickness::Light),
        // ''
        0x0f60b => branch_node(metrics, canvas, BranchNode { up: true, left: true, right: true, ..Default::default() }, Thickness::Light),
        // ''
        0x0f60c => branch_node(metrics, canvas, BranchNode { up: true, down: true, left: true, right: true, filled: true, ..Default::default() }, Thickness::Light),
        // ''
        0x0f60d => branch_node(metrics, canvas, BranchNode { up: true, down: true, left: true, right: true, ..Default::default() }, Thickness::Light),

        _ => unreachable!(),
    }
    Ok(())
}

fn branch_node(metrics: Metrics, canvas: &mut Canvas, node: BranchNode, thickness: Thickness) {
    let thick_px = thickness.height(metrics.box_thickness);
    let float_width = metrics.cell_width as f64;
    let float_height = metrics.cell_height as f64;
    let float_thick = thick_px as f64;

    // Top of horizontal strokes
    let h_top = metrics.cell_height.saturating_sub(thick_px) / 2;
    // Bottom of horizontal strokes
    let h_bottom = h_top.saturating_add(thick_px);
    // Left of vertical strokes
    let v_left = metrics.cell_width.saturating_sub(thick_px) / 2;
    // Right of vertical strokes
    let v_right = v_left.saturating_add(thick_px);

    // We calculate the center of the circle this way
    // to ensure it aligns with box drawing characters
    // since the lines are sometimes off center to
    // make sure they aren't split between pixels.
    let cx: f64 = v_left as f64 + float_thick / 2.0;
    let cy: f64 = h_top as f64 + float_thick / 2.0;
    // The radius needs to be the smallest distance from the center to an edge.
    let r: f64 = cx.min(cy).min((float_width - cx).min(float_height - cy));

    // NOTE: the Zig code acquires the drawing context before drawing the
    // boxes; here it is acquired after them because it borrows the canvas.

    // These @intFromFloat casts shouldn't ever fail since r can never
    // be greater than cx or cy, so when subtracting it from them the
    // result can never be negative.
    if node.up {
        canvas.r#box(
            v_left as i32,
            0,
            v_right as i32,
            (cy - r + float_thick / 2.0).ceil() as i32,
            Color::ON,
        );
    }
    if node.right {
        canvas.r#box(
            (cx + r - float_thick / 2.0).floor() as i32,
            h_top as i32,
            metrics.cell_width as i32,
            h_bottom as i32,
            Color::ON,
        );
    }
    if node.down {
        canvas.r#box(
            v_left as i32,
            (cy + r - float_thick / 2.0).floor() as i32,
            v_right as i32,
            metrics.cell_height as i32,
            Color::ON,
        );
    }
    if node.left {
        canvas.r#box(
            0,
            h_top as i32,
            (cx - r + float_thick / 2.0).ceil() as i32,
            h_bottom as i32,
            Color::ON,
        );
    }

    let mut ctx = canvas.get_context();
    ctx.set_source(Shade::ON.color());
    ctx.set_line_width(float_thick);

    let _ = (|| -> Result<(), DrawFnError> {
        if node.filled {
            ctx.arc(cx, cy, r, 0.0, std::f64::consts::PI * 2.0)?;
            ctx.close_path()?;
            ctx.fill()?;
        } else {
            ctx.arc(cx, cy, r - float_thick / 2.0, 0.0, std::f64::consts::PI * 2.0)?;
            ctx.close_path()?;
            ctx.stroke()?;
        }
        Ok(())
    })();
}

fn fading_line(metrics: Metrics, canvas: &mut Canvas, to: Edge, thickness: Thickness) {
    let thick_px = thickness.height(metrics.box_thickness);
    let float_width = metrics.cell_width as f64;
    let float_height = metrics.cell_height as f64;

    // Top of horizontal strokes
    let h_top = metrics.cell_height.saturating_sub(thick_px) / 2;
    // Bottom of horizontal strokes
    let h_bottom = h_top.saturating_add(thick_px);
    // Left of vertical strokes
    let v_left = metrics.cell_width.saturating_sub(thick_px) / 2;
    // Right of vertical strokes
    let v_right = v_left.saturating_add(thick_px);

    // If we're fading to the top or left, we start with 0.0
    // and increment up as we progress, otherwise we start
    // at 255.0 and increment down (negative).
    let mut color: f64 = match to {
        Edge::Top | Edge::Left => 0.0,
        Edge::Bottom | Edge::Right => 255.0,
    };
    let inc: f64 = 255.0
        / match to {
            Edge::Top => float_height,
            Edge::Bottom => -float_height,
            Edge::Left => float_width,
            Edge::Right => -float_width,
        };

    match to {
        Edge::Top | Edge::Bottom => {
            for y in 0..metrics.cell_height {
                for x in v_left..v_right {
                    canvas.pixel(x as i32, y as i32, Color(color.round() as u8));
                }
                color += inc;
            }
        }
        Edge::Left | Edge::Right => {
            for x in 0..metrics.cell_width {
                for y in h_top..h_bottom {
                    canvas.pixel(x as i32, y as i32, Color(color.round() as u8));
                }
                color += inc;
            }
        }
    }
}
