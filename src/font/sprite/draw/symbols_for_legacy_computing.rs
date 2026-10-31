//! Port of `src/font/sprite/draw/symbols_for_legacy_computing.zig`.
//!
//! STATUS: ported
//!
//! Symbols for Legacy Computing | U+1FB00...U+1FBFF
//! https://en.wikipedia.org/wiki/Symbols_for_Legacy_Computing
//!
//! 🬀 🬁 🬂 🬃 🬄 🬅 🬆 🬇 🬈 🬉 🬊 🬋 🬌 🬍 🬎 🬏
//! 🬐 🬑 🬒 🬓 🬔 🬕 🬖 🬗 🬘 🬙 🬚 🬛 🬜 🬝 🬞 🬟
//! 🬠 🬡 🬢 🬣 🬤 🬥 🬦 🬧 🬨 🬩 🬪 🬫 🬬 🬭 🬮 🬯
//! 🬰 🬱 🬲 🬳 🬴 🬵 🬶 🬷 🬸 🬹 🬺 🬻 🬼 🬽 🬾 🬿
//! 🭀 🭁 🭂 🭃 🭄 🭅 🭆 🭇 🭈 🭉 🭊 🭋 🭌 🭍 🭎 🭏
//! 🭐 🭑 🭒 🭓 🭔 🭕 🭖 🭗 🭘 🭙 🭚 🭛 🭜 🭝 🭞 🭟
//! 🭠 🭡 🭢 🭣 🭤 🭥 🭦 🭧 🭨 🭩 🭪 🭫 🭬 🭭 🭮 🭯
//! 🭰 🭱 🭲 🭳 🭴 🭵 🭶 🭷 🭸 🭹 🭺 🭻 🭼 🭽 🭾 🭿
//! 🮀 🮁 🮂 🮃 🮄 🮅 🮆 🮇 🮈 🮉 🮊 🮋 🮌 🮍 🮎 🮏
//! 🮐 🮑 🮒   🮔 🮕 🮖 🮗 🮘 🮙 🮚 🮛 🮜 🮝 🮞 🮟
//! 🮠 🮡 🮢 🮣 🮤 🮥 🮦 🮧 🮨 🮩 🮪 🮫 🮬 🮭 🮮 🮯
//! 🮰 🮱 🮲 🮳 🮴 🮵 🮶 🮷 🮸 🮹 🮺 🮻 🮼 🮽 🮾 🮿
//! 🯀 🯁 🯂 🯃 🯄 🯅 🯆 🯇 🯈 🯉 🯊 🯋 🯌 🯍 🯎 🯏
//! 🯐 🯑 🯒 🯓 🯔 🯕 🯖 🯗 🯘 🯙 🯚 🯛 🯜 🯝 🯞 🯟
//! 🯠 🯡 🯢 🯣 🯤 🯥 🯦 🯧 🯨 🯩 🯪 🯫 🯬 🯭 🯮 🯯
//! 🯰 🯱 🯲 🯳 🯴 🯵 🯶 🯷 🯸 🯹
//!

use super::block;
use super::common::{
    fill, Alignment, Corner, Edge, Fraction, HorizontalAlignment, Quads, Shade, Thickness,
    VerticalAlignment,
};
use super::geometric_shapes as geo;
use super::r#box::{self, Lines, Style};
use crate::font::metrics::Metrics;
use crate::font::sprite::canvas::{Canvas, Color, FillOptions, Line, Point, Rect};
use crate::font::sprite::face::DrawFnError;

// Utility names for common fractions
const ONE_EIGHTH: f64 = 0.125;
const ONE_QUARTER: f64 = 0.25;
const ONE_THIRD: f64 = 1.0 / 3.0;
const THREE_EIGHTHS: f64 = 0.375;
const HALF: f64 = 0.5;
const FIVE_EIGHTHS: f64 = 0.625;
const TWO_THIRDS: f64 = 2.0 / 3.0;
const THREE_QUARTERS: f64 = 0.75;
const SEVEN_EIGHTHS: f64 = 0.875;

/// (Zig `packed struct(u10)`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SmoothMosaic {
    tl: bool,
    ul: bool,
    ll: bool,
    bl: bool,
    bc: bool,
    br: bool,
    lr: bool,
    ur: bool,
    tr: bool,
    tc: bool,
}

impl SmoothMosaic {
    const fn from(pattern: &[u8; 15]) -> SmoothMosaic {
        SmoothMosaic {
            tl: pattern[0] == b'#',

            ul: pattern[4] == b'#' && (pattern[0] != b'#' || pattern[8] != b'#'),

            ll: pattern[8] == b'#' && (pattern[4] != b'#' || pattern[12] != b'#'),

            bl: pattern[12] == b'#',

            bc: pattern[13] == b'#' && (pattern[12] != b'#' || pattern[14] != b'#'),

            br: pattern[14] == b'#',

            lr: pattern[10] == b'#' && (pattern[14] != b'#' || pattern[6] != b'#'),

            ur: pattern[6] == b'#' && (pattern[10] != b'#' || pattern[2] != b'#'),

            tr: pattern[2] == b'#',

            tc: pattern[1] == b'#' && (pattern[2] != b'#' || pattern[0] != b'#'),
        }
    }
}

/// Sextants
pub fn draw1fb00_1fb3b(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = width;
    let _ = height;

    // (Zig `packed struct(u6) { tl, tr, ml, mr, bl, br }`.)
    debug_assert!((0x1fb00..=0x1fb3b).contains(&cp));
    let idx = cp - 0x1fb00;
    let bits = (idx + (idx / 0x14) + 1) as u8;
    debug_assert!(bits < 64);
    let bit = |i: u8| (bits >> i) & 1 == 1;
    let (tl, tr, ml, mr, bl, br) = (bit(0), bit(1), bit(2), bit(3), bit(4), bit(5));
    if tl {
        fill(metrics, canvas, Fraction::Zero, Fraction::Half, Fraction::Zero, Fraction::OneThird);
    }
    if tr {
        fill(metrics, canvas, Fraction::Half, Fraction::Full, Fraction::Zero, Fraction::OneThird);
    }
    if ml {
        fill(metrics, canvas, Fraction::Zero, Fraction::Half, Fraction::OneThird, Fraction::TwoThirds);
    }
    if mr {
        fill(metrics, canvas, Fraction::Half, Fraction::Full, Fraction::OneThird, Fraction::TwoThirds);
    }
    if bl {
        fill(metrics, canvas, Fraction::Zero, Fraction::Half, Fraction::TwoThirds, Fraction::End);
    }
    if br {
        fill(metrics, canvas, Fraction::Half, Fraction::Full, Fraction::TwoThirds, Fraction::End);
    }
    Ok(())
}

/// Smooth Mosaics
pub fn draw1fb3c_1fb67(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = width;
    let _ = height;

    // Hand written lookup table for these shapes since I couldn't
    // determine any sort of mathematical pattern in the codepoints.
    let mosaic: SmoothMosaic = match cp {
        // '🬼'
        0x1fb3c => SmoothMosaic::from(b"...\n...\n#..\n##."),
        // '🬽'
        0x1fb3d => SmoothMosaic::from(b"...\n...\n#\\.\n###"),
        // '🬾'
        0x1fb3e => SmoothMosaic::from(b"...\n#..\n#\\.\n##."),
        // '🬿'
        0x1fb3f => SmoothMosaic::from(b"...\n#..\n##.\n###"),
        // '🭀'
        0x1fb40 => SmoothMosaic::from(b"#..\n#..\n##.\n##."),

        // '🭁'
        0x1fb41 => SmoothMosaic::from(b"/##\n###\n###\n###"),
        // '🭂'
        0x1fb42 => SmoothMosaic::from(b"./#\n###\n###\n###"),
        // '🭃'
        0x1fb43 => SmoothMosaic::from(b".##\n.##\n###\n###"),
        // '🭄'
        0x1fb44 => SmoothMosaic::from(b"..#\n.##\n###\n###"),
        // '🭅'
        0x1fb45 => SmoothMosaic::from(b".##\n.##\n.##\n###"),
        // '🭆'
        0x1fb46 => SmoothMosaic::from(b"...\n./#\n###\n###"),

        // '🭇'
        0x1fb47 => SmoothMosaic::from(b"...\n...\n..#\n.##"),
        // '🭈'
        0x1fb48 => SmoothMosaic::from(b"...\n...\n./#\n###"),
        // '🭉'
        0x1fb49 => SmoothMosaic::from(b"...\n..#\n./#\n.##"),
        // '🭊'
        0x1fb4a => SmoothMosaic::from(b"...\n..#\n.##\n###"),
        // '🭋'
        0x1fb4b => SmoothMosaic::from(b"..#\n..#\n.##\n.##"),

        // '🭌'
        0x1fb4c => SmoothMosaic::from(b"##\\\n###\n###\n###"),
        // '🭍'
        0x1fb4d => SmoothMosaic::from(b"#\\.\n###\n###\n###"),
        // '🭎'
        0x1fb4e => SmoothMosaic::from(b"##.\n##.\n###\n###"),
        // '🭏'
        0x1fb4f => SmoothMosaic::from(b"#..\n##.\n###\n###"),
        // '🭐'
        0x1fb50 => SmoothMosaic::from(b"##.\n##.\n##.\n###"),
        // '🭑'
        0x1fb51 => SmoothMosaic::from(b"...\n#\\.\n###\n###"),

        // '🭒'
        0x1fb52 => SmoothMosaic::from(b"###\n###\n###\n\\##"),
        // '🭓'
        0x1fb53 => SmoothMosaic::from(b"###\n###\n###\n.\\#"),
        // '🭔'
        0x1fb54 => SmoothMosaic::from(b"###\n###\n.##\n.##"),
        // '🭕'
        0x1fb55 => SmoothMosaic::from(b"###\n###\n.##\n..#"),
        // '🭖'
        0x1fb56 => SmoothMosaic::from(b"###\n.##\n.##\n.##"),

        // '🭗'
        0x1fb57 => SmoothMosaic::from(b"##.\n#..\n...\n..."),
        // '🭘'
        0x1fb58 => SmoothMosaic::from(b"###\n#/.\n...\n..."),
        // '🭙'
        0x1fb59 => SmoothMosaic::from(b"##.\n#/.\n#..\n..."),
        // '🭚'
        0x1fb5a => SmoothMosaic::from(b"###\n##.\n#..\n..."),
        // '🭛'
        0x1fb5b => SmoothMosaic::from(b"##.\n##.\n#..\n#.."),

        // '🭜'
        0x1fb5c => SmoothMosaic::from(b"###\n###\n#/.\n..."),
        // '🭝'
        0x1fb5d => SmoothMosaic::from(b"###\n###\n###\n##/"),
        // '🭞'
        0x1fb5e => SmoothMosaic::from(b"###\n###\n###\n#/."),
        // '🭟'
        0x1fb5f => SmoothMosaic::from(b"###\n###\n##.\n##."),
        // '🭠'
        0x1fb60 => SmoothMosaic::from(b"###\n###\n##.\n#.."),
        // '🭡'
        0x1fb61 => SmoothMosaic::from(b"###\n##.\n##.\n##."),

        // '🭢'
        0x1fb62 => SmoothMosaic::from(b".##\n..#\n...\n..."),
        // '🭣'
        0x1fb63 => SmoothMosaic::from(b"###\n.\\#\n...\n..."),
        // '🭤'
        0x1fb64 => SmoothMosaic::from(b".##\n.\\#\n..#\n..."),
        // '🭥'
        0x1fb65 => SmoothMosaic::from(b"###\n.##\n..#\n..."),
        // '🭦'
        0x1fb66 => SmoothMosaic::from(b".##\n.##\n..#\n..#"),
        // '🭧'
        0x1fb67 => SmoothMosaic::from(b"###\n###\n.\\#\n..."),
        _ => unreachable!(),
    };

    let top: f64 = 0.0;
    let upper: f64 = Fraction::OneThird.float(metrics.cell_height);
    let lower: f64 = Fraction::TwoThirds.float(metrics.cell_height);
    let bottom: f64 = metrics.cell_height as f64;
    let left: f64 = 0.0;
    let center: f64 = Fraction::Half.float(metrics.cell_width);
    let right: f64 = metrics.cell_width as f64;

    let mut path = canvas.static_path(12); // nodes.len = 0
    if mosaic.tl {
        path.line_to(left, top); // +1, nodes.len = 1
    }
    if mosaic.ul {
        path.line_to(left, upper); // +1, nodes.len = 2
    }
    if mosaic.ll {
        path.line_to(left, lower); // +1, nodes.len = 3
    }
    if mosaic.bl {
        path.line_to(left, bottom); // +1, nodes.len = 4
    }
    if mosaic.bc {
        path.line_to(center, bottom); // +1, nodes.len = 5
    }
    if mosaic.br {
        path.line_to(right, bottom); // +1, nodes.len = 6
    }
    if mosaic.lr {
        path.line_to(right, lower); // +1, nodes.len = 7
    }
    if mosaic.ur {
        path.line_to(right, upper); // +1, nodes.len = 8
    }
    if mosaic.tr {
        path.line_to(right, top); // +1, nodes.len = 9
    }
    if mosaic.tc {
        path.line_to(center, top); // +1, nodes.len = 10
    }
    path.close(); // +2, nodes.len = 12

    canvas.fill_path(&path, FillOptions::default(), Color::ON)
}

pub fn draw1fb68_1fb6f(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = width;
    let _ = height;

    match cp {
        // '🭨'
        0x1fb68 => {
            edge_triangle(metrics, canvas, Edge::Left)?;
            canvas.invert();
            // Set the clip so we don't include anything outside of the cell.
            canvas.clip_left = canvas.padding_x;
            canvas.clip_right = canvas.padding_x;
            canvas.clip_top = canvas.padding_y;
            canvas.clip_bottom = canvas.padding_y;
        },
        // '🭩'
        0x1fb69 => {
            edge_triangle(metrics, canvas, Edge::Top)?;
            canvas.invert();
            // Set the clip so we don't include anything outside of the cell.
            canvas.clip_left = canvas.padding_x;
            canvas.clip_right = canvas.padding_x;
            canvas.clip_top = canvas.padding_y;
            canvas.clip_bottom = canvas.padding_y;
        },
        // '🭪'
        0x1fb6a => {
            edge_triangle(metrics, canvas, Edge::Right)?;
            canvas.invert();
            // Set the clip so we don't include anything outside of the cell.
            canvas.clip_left = canvas.padding_x;
            canvas.clip_right = canvas.padding_x;
            canvas.clip_top = canvas.padding_y;
            canvas.clip_bottom = canvas.padding_y;
        },
        // '🭫'
        0x1fb6b => {
            edge_triangle(metrics, canvas, Edge::Bottom)?;
            canvas.invert();
            // Set the clip so we don't include anything outside of the cell.
            canvas.clip_left = canvas.padding_x;
            canvas.clip_right = canvas.padding_x;
            canvas.clip_top = canvas.padding_y;
            canvas.clip_bottom = canvas.padding_y;
        },
        // '🭬'
        0x1fb6c => edge_triangle(metrics, canvas, Edge::Left)?,
        // '🭭'
        0x1fb6d => edge_triangle(metrics, canvas, Edge::Top)?,
        // '🭮'
        0x1fb6e => edge_triangle(metrics, canvas, Edge::Right)?,
        // '🭯'
        0x1fb6f => edge_triangle(metrics, canvas, Edge::Bottom)?,

        _ => unreachable!(),
    }
    Ok(())
}

/// Vertical one eighth blocks
pub fn draw1fb70_1fb75(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = width;
    let _ = height;

    let n = (cp + 1 - 0x1fb70) as usize;

    fill(
        metrics,
        canvas,
        Fraction::EIGHTHS[n],
        Fraction::EIGHTHS[n + 1],
        Fraction::Top,
        Fraction::Bottom,
    );
    Ok(())
}

/// Horizontal one eighth blocks
pub fn draw1fb76_1fb7b(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = width;
    let _ = height;

    let n = (cp + 1 - 0x1fb76) as usize;

    fill(
        metrics,
        canvas,
        Fraction::Left,
        Fraction::Right,
        Fraction::EIGHTHS[n],
        Fraction::EIGHTHS[n + 1],
    );
    Ok(())
}

pub fn draw1fb7c_1fb97(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    match cp {

        // '🭼' LEFT AND LOWER ONE EIGHTH BLOCK
        0x1fb7c => {
            block::block(metrics, canvas, Alignment::LEFT, ONE_EIGHTH, 1.0);
            block::block(metrics, canvas, Alignment::LOWER, 1.0, ONE_EIGHTH);
        },
        // '🭽' LEFT AND UPPER ONE EIGHTH BLOCK
        0x1fb7d => {
            block::block(metrics, canvas, Alignment::LEFT, ONE_EIGHTH, 1.0);
            block::block(metrics, canvas, Alignment::UPPER, 1.0, ONE_EIGHTH);
        },
        // '🭾' RIGHT AND UPPER ONE EIGHTH BLOCK
        0x1fb7e => {
            block::block(metrics, canvas, Alignment::RIGHT, ONE_EIGHTH, 1.0);
            block::block(metrics, canvas, Alignment::UPPER, 1.0, ONE_EIGHTH);
        },
        // '🭿' RIGHT AND LOWER ONE EIGHTH BLOCK
        0x1fb7f => {
            block::block(metrics, canvas, Alignment::RIGHT, ONE_EIGHTH, 1.0);
            block::block(metrics, canvas, Alignment::LOWER, 1.0, ONE_EIGHTH);
        },
        // '🮀' UPPER AND LOWER ONE EIGHTH BLOCK
        0x1fb80 => {
            block::block(metrics, canvas, Alignment::UPPER, 1.0, ONE_EIGHTH);
            block::block(metrics, canvas, Alignment::LOWER, 1.0, ONE_EIGHTH);
        },
        // '🮁' Horizontal One Eighth Block 1358
        0x1fb81 => {
            // We just call the draw function for each of the relevant codepoints.
            // The first codepoint is actually a lie, it's before the range, but
            // we need it to get the first (0th) block position. This might be a
            // bit brittle, oh well, if it breaks we can fix it.
            draw1fb76_1fb7b(0x1fb74 + 1, canvas, width, height, metrics)?;
            draw1fb76_1fb7b(0x1fb74 + 3, canvas, width, height, metrics)?;
            draw1fb76_1fb7b(0x1fb74 + 5, canvas, width, height, metrics)?;
            draw1fb76_1fb7b(0x1fb74 + 8, canvas, width, height, metrics)?;
        },

        // '🮂' UPPER ONE QUARTER BLOCK
        0x1fb82 => block::block(metrics, canvas, Alignment::UPPER, 1.0, ONE_QUARTER),
        // '🮃' UPPER THREE EIGHTHS BLOCK
        0x1fb83 => block::block(metrics, canvas, Alignment::UPPER, 1.0, THREE_EIGHTHS),
        // '🮄' UPPER FIVE EIGHTHS BLOCK
        0x1fb84 => block::block(metrics, canvas, Alignment::UPPER, 1.0, FIVE_EIGHTHS),
        // '🮅' UPPER THREE QUARTERS BLOCK
        0x1fb85 => block::block(metrics, canvas, Alignment::UPPER, 1.0, THREE_QUARTERS),
        // '🮆' UPPER SEVEN EIGHTHS BLOCK
        0x1fb86 => block::block(metrics, canvas, Alignment::UPPER, 1.0, SEVEN_EIGHTHS),

        // '🮇' RIGHT ONE QUARTER BLOCK
        0x1fb87 => block::block(metrics, canvas, Alignment::RIGHT, ONE_QUARTER, 1.0),
        // '🮈' RIGHT THREE EIGHTHS BLOCK
        0x1fb88 => block::block(metrics, canvas, Alignment::RIGHT, THREE_EIGHTHS, 1.0),
        // '🮉' RIGHT FIVE EIGHTHS BLOCK
        0x1fb89 => block::block(metrics, canvas, Alignment::RIGHT, FIVE_EIGHTHS, 1.0),
        // '🮊' RIGHT THREE QUARTERS BLOCK
        0x1fb8a => block::block(metrics, canvas, Alignment::RIGHT, THREE_QUARTERS, 1.0),
        // '🮋' RIGHT SEVEN EIGHTHS BLOCK/
        0x1fb8b => block::block(metrics, canvas, Alignment::RIGHT, SEVEN_EIGHTHS, 1.0),

        // '🮌'
        0x1fb8c => block::block_shade(metrics, canvas, Alignment::LEFT, HALF, 1.0, Shade::MEDIUM),
        // '🮍'
        0x1fb8d => block::block_shade(metrics, canvas, Alignment::RIGHT, HALF, 1.0, Shade::MEDIUM),
        // '🮎'
        0x1fb8e => block::block_shade(metrics, canvas, Alignment::UPPER, 1.0, HALF, Shade::MEDIUM),
        // '🮏'
        0x1fb8f => block::block_shade(metrics, canvas, Alignment::LOWER, 1.0, HALF, Shade::MEDIUM),

        // '🮐'
        0x1fb90 => block::full_block_shade(metrics, canvas, Shade::MEDIUM),
        // '🮑'
        0x1fb91 => {
            block::full_block_shade(metrics, canvas, Shade::MEDIUM);
            block::block(metrics, canvas, Alignment::UPPER, 1.0, HALF);
        },
        // '🮒'
        0x1fb92 => {
            block::full_block_shade(metrics, canvas, Shade::MEDIUM);
            block::block(metrics, canvas, Alignment::LOWER, 1.0, HALF);
        },
        0x1fb93 => {
            // NOTE: This codepoint is currently un-allocated, it's a hole
            //       in the unicode block, so it's safe to just render it
            //       as an empty glyph, probably.
        },
        // '🮔'
        0x1fb94 => {
            block::full_block_shade(metrics, canvas, Shade::MEDIUM);
            block::block(metrics, canvas, Alignment::RIGHT, HALF, 1.0);
        },
        // '🮕'
        0x1fb95 => checkerboard_fill(metrics, canvas, 0),
        // '🮖'
        0x1fb96 => checkerboard_fill(metrics, canvas, 1),
        // '🮗'
        0x1fb97 => {
            canvas.r#box(
                0,
                (height / 4) as i32,
                width as i32,
                (2 * height / 4) as i32,
                Color::ON,
            );
            canvas.r#box(
                0,
                (3 * height / 4) as i32,
                width as i32,
                height as i32,
                Color::ON,
            );
        },

        _ => unreachable!(),
    }
    Ok(())
}

/// Upper Left to Lower Right Fill
/// 🮘
pub fn draw1fb98(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = cp;
    let _ = width;
    let _ = height;

    // Set the clip so we don't include anything outside of the cell.
    canvas.clip_left = canvas.padding_x;
    canvas.clip_right = canvas.padding_x;
    canvas.clip_top = canvas.padding_y;
    canvas.clip_bottom = canvas.padding_y;

    // TODO: This doesn't align properly for most cell sizes, fix that.

    let thick_px = Thickness::Light.height(metrics.box_thickness);
    let line_count = metrics.cell_width / (2 * thick_px);

    let float_width = metrics.cell_width as f64;
    let float_height = metrics.cell_height as f64;
    let float_thick = thick_px as f64;
    let stride = (float_width / line_count as f64).round();

    for i_ in 0..(line_count * 2 + 1) {
        let i = i_ as i32 - line_count as i32;
        let top_x = i as f64 * stride;
        let bottom_x = float_width + top_x;
        let _ = canvas.line(
            Line {
                p0: Point { x: top_x, y: 0.0 },
                p1: Point {
                    x: bottom_x,
                    y: float_height,
                },
            },
            float_thick,
            Color::ON,
        );
    }
    Ok(())
}

/// Upper Right to Lower Left Fill
/// 🮙
pub fn draw1fb99(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = cp;
    let _ = width;
    let _ = height;

    // Set the clip so we don't include anything outside of the cell.
    canvas.clip_left = canvas.padding_x;
    canvas.clip_right = canvas.padding_x;
    canvas.clip_top = canvas.padding_y;
    canvas.clip_bottom = canvas.padding_y;

    // TODO: This doesn't align properly for most cell sizes, fix that.

    let thick_px = Thickness::Light.height(metrics.box_thickness);
    let line_count = metrics.cell_width / (2 * thick_px);

    let float_width = metrics.cell_width as f64;
    let float_height = metrics.cell_height as f64;
    let float_thick = thick_px as f64;
    let stride = (float_width / line_count as f64).round();

    for i_ in 0..(line_count * 2 + 1) {
        let i = i_ as i32 - line_count as i32;
        let bottom_x = i as f64 * stride;
        let top_x = float_width + bottom_x;
        let _ = canvas.line(
            Line {
                p0: Point { x: top_x, y: 0.0 },
                p1: Point {
                    x: bottom_x,
                    y: float_height,
                },
            },
            float_thick,
            Color::ON,
        );
    }
    Ok(())
}

pub fn draw1fb9a_1fb9f(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = width;
    let _ = height;

    match cp {
        // '🮚'
        0x1fb9a => {
            edge_triangle(metrics, canvas, Edge::Top)?;
            edge_triangle(metrics, canvas, Edge::Bottom)?;
        },
        // '🮛'
        0x1fb9b => {
            edge_triangle(metrics, canvas, Edge::Left)?;
            edge_triangle(metrics, canvas, Edge::Right)?;
        },
        // '🮜'
        0x1fb9c => geo::corner_triangle_shade(metrics, canvas, Corner::Tl, Shade::MEDIUM)?,
        // '🮝'
        0x1fb9d => geo::corner_triangle_shade(metrics, canvas, Corner::Tr, Shade::MEDIUM)?,
        // '🮞'
        0x1fb9e => geo::corner_triangle_shade(metrics, canvas, Corner::Br, Shade::MEDIUM)?,
        // '🮟'
        0x1fb9f => geo::corner_triangle_shade(metrics, canvas, Corner::Bl, Shade::MEDIUM)?,

        _ => unreachable!(),
    }
    Ok(())
}

pub fn draw1fba0_1fbae(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = width;
    let _ = height;

    match cp {
        // '🮠'
        0x1fba0 => corner_diagonal_lines(metrics, canvas, Quads { tl: true, ..Default::default() }),
        // '🮡'
        0x1fba1 => corner_diagonal_lines(metrics, canvas, Quads { tr: true, ..Default::default() }),
        // '🮢'
        0x1fba2 => corner_diagonal_lines(metrics, canvas, Quads { bl: true, ..Default::default() }),
        // '🮣'
        0x1fba3 => corner_diagonal_lines(metrics, canvas, Quads { br: true, ..Default::default() }),
        // '🮤'
        0x1fba4 => corner_diagonal_lines(metrics, canvas, Quads { tl: true, bl: true, ..Default::default() }),
        // '🮥'
        0x1fba5 => corner_diagonal_lines(metrics, canvas, Quads { tr: true, br: true, ..Default::default() }),
        // '🮦'
        0x1fba6 => corner_diagonal_lines(metrics, canvas, Quads { bl: true, br: true, ..Default::default() }),
        // '🮧'
        0x1fba7 => corner_diagonal_lines(metrics, canvas, Quads { tl: true, tr: true, ..Default::default() }),
        // '🮨'
        0x1fba8 => corner_diagonal_lines(metrics, canvas, Quads { tl: true, br: true, ..Default::default() }),
        // '🮩'
        0x1fba9 => corner_diagonal_lines(metrics, canvas, Quads { tr: true, bl: true, ..Default::default() }),
        // '🮪'
        0x1fbaa => corner_diagonal_lines(metrics, canvas, Quads { tr: true, bl: true, br: true, ..Default::default() }),
        // '🮫'
        0x1fbab => corner_diagonal_lines(metrics, canvas, Quads { tl: true, bl: true, br: true, ..Default::default() }),
        // '🮬'
        0x1fbac => corner_diagonal_lines(metrics, canvas, Quads { tl: true, tr: true, br: true, ..Default::default() }),
        // '🮭'
        0x1fbad => corner_diagonal_lines(metrics, canvas, Quads { tl: true, tr: true, bl: true, ..Default::default() }),
        // '🮮'
        0x1fbae => corner_diagonal_lines(metrics, canvas, Quads { tl: true, tr: true, bl: true, br: true, ..Default::default() }),

        _ => unreachable!(),
    }
    Ok(())
}

/// 🮯
pub fn draw1fbaf(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = cp;
    let _ = width;
    let _ = height;

    r#box::lines_char(metrics, canvas, Lines { up: Style::Heavy, down: Style::Heavy, left: Style::Light, right: Style::Light, ..Lines::NONE });
    Ok(())
}

/// 🮽
pub fn draw1fbbd(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = cp;
    let _ = width;
    let _ = height;

    r#box::light_diagonal_cross(metrics, canvas);
    canvas.invert();
    // Set the clip so we don't include anything outside of the cell.
    canvas.clip_left = canvas.padding_x;
    canvas.clip_right = canvas.padding_x;
    canvas.clip_top = canvas.padding_y;
    canvas.clip_bottom = canvas.padding_y;
    Ok(())
}

/// 🮾
pub fn draw1fbbe(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = cp;
    let _ = width;
    let _ = height;

    corner_diagonal_lines(metrics, canvas, Quads { br: true, ..Default::default() });
    canvas.invert();
    // Set the clip so we don't include anything outside of the cell.
    canvas.clip_left = canvas.padding_x;
    canvas.clip_right = canvas.padding_x;
    canvas.clip_top = canvas.padding_y;
    canvas.clip_bottom = canvas.padding_y;
    Ok(())
}

/// 🮿
pub fn draw1fbbf(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = cp;
    let _ = width;
    let _ = height;

    corner_diagonal_lines(metrics, canvas, Quads { tl: true, tr: true, bl: true, br: true, ..Default::default() });
    canvas.invert();
    // Set the clip so we don't include anything outside of the cell.
    canvas.clip_left = canvas.padding_x;
    canvas.clip_right = canvas.padding_x;
    canvas.clip_top = canvas.padding_y;
    canvas.clip_bottom = canvas.padding_y;
    Ok(())
}

/// 🯎
pub fn draw1fbce(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = cp;
    let _ = width;
    let _ = height;

    block::block(metrics, canvas, Alignment::LEFT, TWO_THIRDS, 1.0);
    Ok(())
}

// 🯏
pub fn draw1fbcf(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = cp;
    let _ = width;
    let _ = height;

    block::block(metrics, canvas, Alignment::LEFT, ONE_THIRD, 1.0);
    Ok(())
}

/// Cell diagonals.
pub fn draw1fbd0_1fbdf(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = width;
    let _ = height;

    match cp {
        // '🯐'
        0x1fbd0 => cell_diagonal(metrics, canvas, Alignment::MIDDLE_RIGHT, Alignment::LOWER_LEFT),
        // '🯑'
        0x1fbd1 => cell_diagonal(metrics, canvas, Alignment::UPPER_RIGHT, Alignment::MIDDLE_LEFT),
        // '🯒'
        0x1fbd2 => cell_diagonal(metrics, canvas, Alignment::UPPER_LEFT, Alignment::MIDDLE_RIGHT),
        // '🯓'
        0x1fbd3 => cell_diagonal(metrics, canvas, Alignment::MIDDLE_LEFT, Alignment::LOWER_RIGHT),
        // '🯔'
        0x1fbd4 => cell_diagonal(metrics, canvas, Alignment::UPPER_LEFT, Alignment::LOWER_CENTER),
        // '🯕'
        0x1fbd5 => cell_diagonal(metrics, canvas, Alignment::UPPER_CENTER, Alignment::LOWER_RIGHT),
        // '🯖'
        0x1fbd6 => cell_diagonal(metrics, canvas, Alignment::UPPER_RIGHT, Alignment::LOWER_CENTER),
        // '🯗'
        0x1fbd7 => cell_diagonal(metrics, canvas, Alignment::UPPER_CENTER, Alignment::LOWER_LEFT),
        // '🯘'
        0x1fbd8 => {
            cell_diagonal(metrics, canvas, Alignment::UPPER_LEFT, Alignment::MIDDLE_CENTER);
            cell_diagonal(metrics, canvas, Alignment::MIDDLE_CENTER, Alignment::UPPER_RIGHT);
        },
        // '🯙'
        0x1fbd9 => {
            cell_diagonal(metrics, canvas, Alignment::UPPER_RIGHT, Alignment::MIDDLE_CENTER);
            cell_diagonal(metrics, canvas, Alignment::MIDDLE_CENTER, Alignment::LOWER_RIGHT);
        },
        // '🯚'
        0x1fbda => {
            cell_diagonal(metrics, canvas, Alignment::LOWER_LEFT, Alignment::MIDDLE_CENTER);
            cell_diagonal(metrics, canvas, Alignment::MIDDLE_CENTER, Alignment::LOWER_RIGHT);
        },
        // '🯛'
        0x1fbdb => {
            cell_diagonal(metrics, canvas, Alignment::UPPER_LEFT, Alignment::MIDDLE_CENTER);
            cell_diagonal(metrics, canvas, Alignment::MIDDLE_CENTER, Alignment::LOWER_LEFT);
        },
        // '🯜'
        0x1fbdc => {
            cell_diagonal(metrics, canvas, Alignment::UPPER_LEFT, Alignment::LOWER_CENTER);
            cell_diagonal(metrics, canvas, Alignment::LOWER_CENTER, Alignment::UPPER_RIGHT);
        },
        // '🯝'
        0x1fbdd => {
            cell_diagonal(metrics, canvas, Alignment::UPPER_RIGHT, Alignment::MIDDLE_LEFT);
            cell_diagonal(metrics, canvas, Alignment::MIDDLE_LEFT, Alignment::LOWER_RIGHT);
        },
        // '🯞'
        0x1fbde => {
            cell_diagonal(metrics, canvas, Alignment::LOWER_LEFT, Alignment::UPPER_CENTER);
            cell_diagonal(metrics, canvas, Alignment::UPPER_CENTER, Alignment::LOWER_RIGHT);
        },
        // '🯟'
        0x1fbdf => {
            cell_diagonal(metrics, canvas, Alignment::UPPER_LEFT, Alignment::MIDDLE_RIGHT);
            cell_diagonal(metrics, canvas, Alignment::MIDDLE_RIGHT, Alignment::LOWER_LEFT);
        },

        _ => unreachable!(),
    }
    Ok(())
}

pub fn draw1fbe0_1fbef(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = width;
    let _ = height;

    match cp {
        // '🯠'
        0x1fbe0 => circle(metrics, canvas, Alignment::TOP, false),
        // '🯡'
        0x1fbe1 => circle(metrics, canvas, Alignment::RIGHT, false),
        // '🯢'
        0x1fbe2 => circle(metrics, canvas, Alignment::BOTTOM, false),
        // '🯣'
        0x1fbe3 => circle(metrics, canvas, Alignment::LEFT, false),
        // '🯤'
        0x1fbe4 => block::block(metrics, canvas, Alignment::UPPER_CENTER, 0.5, 0.5),
        // '🯥'
        0x1fbe5 => block::block(metrics, canvas, Alignment::LOWER_CENTER, 0.5, 0.5),
        // '🯦'
        0x1fbe6 => block::block(metrics, canvas, Alignment::MIDDLE_LEFT, 0.5, 0.5),
        // '🯧'
        0x1fbe7 => block::block(metrics, canvas, Alignment::MIDDLE_RIGHT, 0.5, 0.5),
        // '🯨'
        0x1fbe8 => circle(metrics, canvas, Alignment::TOP, true),
        // '🯩'
        0x1fbe9 => circle(metrics, canvas, Alignment::RIGHT, true),
        // '🯪'
        0x1fbea => circle(metrics, canvas, Alignment::BOTTOM, true),
        // '🯫'
        0x1fbeb => circle(metrics, canvas, Alignment::LEFT, true),
        // '🯬'
        0x1fbec => circle(metrics, canvas, Alignment::TOP_RIGHT, true),
        // '🯭'
        0x1fbed => circle(metrics, canvas, Alignment::BOTTOM_LEFT, true),
        // '🯮'
        0x1fbee => circle(metrics, canvas, Alignment::BOTTOM_RIGHT, true),
        // '🯯'
        0x1fbef => circle(metrics, canvas, Alignment::TOP_LEFT, true),

        _ => unreachable!(),
    }
    Ok(())
}

fn edge_triangle(metrics: Metrics, canvas: &mut Canvas, edge: Edge) -> Result<(), DrawFnError> {
    let upper: f64 = 0.0;
    let middle: f64 = (metrics.cell_height as f64 / 2.0).round();
    let lower: f64 = metrics.cell_height as f64;
    let left: f64 = 0.0;
    let center: f64 = (metrics.cell_width as f64 / 2.0).round();
    let right: f64 = metrics.cell_width as f64;

    let (x0, y0, x1, y1) = match edge {
        Edge::Top => (right, upper, left, upper),
        Edge::Left => (left, upper, left, lower),
        Edge::Bottom => (left, lower, right, lower),
        Edge::Right => (right, lower, right, upper),
    };

    let mut path = canvas.static_path(5); // nodes.len = 0
    path.move_to(center, middle); // +1, nodes.len = 1
    path.line_to(x0, y0); // +1, nodes.len = 2
    path.line_to(x1, y1); // +1, nodes.len = 3
    path.close(); // +2, nodes.len = 5

    canvas.fill_path(&path, FillOptions::default(), Color::ON)
}

fn corner_diagonal_lines(metrics: Metrics, canvas: &mut Canvas, corners: Quads) {
    let thick_px = Thickness::Light.height(metrics.box_thickness);

    let float_width = metrics.cell_width as f64;
    let float_height = metrics.cell_height as f64;
    let float_thick = thick_px as f64;
    let center_x = (metrics.cell_width / 2 + metrics.cell_width % 2) as f64;
    let center_y = (metrics.cell_height / 2 + metrics.cell_height % 2) as f64;

    let line = |x0: f64, y0: f64, x1: f64, y1: f64| Line {
        p0: Point { x: x0, y: y0 },
        p1: Point { x: x1, y: y1 },
    };

    if corners.tl {
        let _ = canvas.line(line(center_x, 0.0, 0.0, center_y), float_thick, Color::ON);
    }

    if corners.tr {
        let _ = canvas.line(line(center_x, 0.0, float_width, center_y), float_thick, Color::ON);
    }

    if corners.bl {
        let _ = canvas.line(line(center_x, float_height, 0.0, center_y), float_thick, Color::ON);
    }

    if corners.br {
        let _ = canvas.line(
            line(center_x, float_height, float_width, center_y),
            float_thick,
            Color::ON,
        );
    }
}

fn cell_diagonal(metrics: Metrics, canvas: &mut Canvas, from: Alignment, to: Alignment) {
    let float_width = metrics.cell_width as f64;
    let float_height = metrics.cell_height as f64;

    let x0: f64 = match from.horizontal {
        HorizontalAlignment::Left => 0.0,
        HorizontalAlignment::Right => float_width,
        HorizontalAlignment::Center => float_width / 2.0,
    };
    let y0: f64 = match from.vertical {
        VerticalAlignment::Top => 0.0,
        VerticalAlignment::Bottom => float_height,
        VerticalAlignment::Middle => float_height / 2.0,
    };
    let x1: f64 = match to.horizontal {
        HorizontalAlignment::Left => 0.0,
        HorizontalAlignment::Right => float_width,
        HorizontalAlignment::Center => float_width / 2.0,
    };
    let y1: f64 = match to.vertical {
        VerticalAlignment::Top => 0.0,
        VerticalAlignment::Bottom => float_height,
        VerticalAlignment::Middle => float_height / 2.0,
    };

    let _ = canvas.line(
        Line {
            p0: Point { x: x0, y: y0 },
            p1: Point { x: x1, y: y1 },
        },
        Thickness::Light.height(metrics.box_thickness) as f64,
        Color::ON,
    );
}

fn checkerboard_fill(metrics: Metrics, canvas: &mut Canvas, parity: u8) {
    let float_width = metrics.cell_width as f64;
    let float_height = metrics.cell_height as f64;
    let x_size: usize = 4;
    let y_size: usize = (4.0 * (float_height / float_width)).round() as usize;
    for x in 0..x_size {
        let x0 = (metrics.cell_width as usize * x) / x_size;
        let x1 = (metrics.cell_width as usize * (x + 1)) / x_size;
        for y in 0..y_size {
            let y0 = (metrics.cell_height as usize * y) / y_size;
            let y1 = (metrics.cell_height as usize * (y + 1)) / y_size;
            if (x + y) % 2 == parity as usize {
                canvas.rect(
                    Rect {
                        x: x0 as i32,
                        y: y0 as i32,
                        width: x1.saturating_sub(x0) as i32,
                        height: y1.saturating_sub(y0) as i32,
                    },
                    Color::ON,
                );
            }
        }
    }
}

pub fn circle(metrics: Metrics, canvas: &mut Canvas, position: Alignment, filled: bool) {
    // Set the clip so we don't include anything outside of the cell.
    canvas.clip_left = canvas.padding_x;
    canvas.clip_right = canvas.padding_x;
    canvas.clip_top = canvas.padding_y;
    canvas.clip_bottom = canvas.padding_y;

    let float_width = metrics.cell_width as f64;
    let float_height = metrics.cell_height as f64;

    let x: f64 = match position.horizontal {
        HorizontalAlignment::Left => 0.0,
        HorizontalAlignment::Right => float_width,
        HorizontalAlignment::Center => float_width / 2.0,
    };
    let y: f64 = match position.vertical {
        VerticalAlignment::Top => 0.0,
        VerticalAlignment::Bottom => float_height,
        VerticalAlignment::Middle => float_height / 2.0,
    };
    let r: f64 = 0.5 * float_width.min(float_height);

    let mut ctx = canvas.get_context();
    ctx.set_source(Shade::ON.color());
    ctx.set_line_width(Thickness::Light.height(metrics.box_thickness) as f64);

    let _ = (|| -> Result<(), DrawFnError> {
        if filled {
            ctx.arc(x, y, r, 0.0, std::f64::consts::PI * 2.0)?;
            ctx.close_path()?;
            ctx.fill()?;
        } else {
            let lw = ctx.line_width;
            ctx.arc(x, y, r - lw / 2.0, 0.0, std::f64::consts::PI * 2.0)?;
            ctx.close_path()?;
            ctx.stroke()?;
        }
        Ok(())
    })();
}
