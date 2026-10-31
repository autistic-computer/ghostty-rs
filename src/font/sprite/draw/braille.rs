//! Port of `src/font/sprite/draw/braille.zig`.
//!
//! STATUS: ported
//!
//! Braille Patterns | U+2800...U+28FF
//! https://en.wikipedia.org/wiki/Braille_Patterns
//!
//! (6 dot patterns)
//! ⠀ ⠁ ⠂ ⠃ ⠄ ⠅ ⠆ ⠇ ⠈ ⠉ ⠊ ⠋ ⠌ ⠍ ⠎ ⠏
//! ⠐ ⠑ ⠒ ⠓ ⠔ ⠕ ⠖ ⠗ ⠘ ⠙ ⠚ ⠛ ⠜ ⠝ ⠞ ⠟
//! ⠠ ⠡ ⠢ ⠣ ⠤ ⠥ ⠦ ⠧ ⠨ ⠩ ⠪ ⠫ ⠬ ⠭ ⠮ ⠯
//! ⠰ ⠱ ⠲ ⠳ ⠴ ⠵ ⠶ ⠷ ⠸ ⠹ ⠺ ⠻ ⠼ ⠽ ⠾ ⠿
//!
//! (8 dot patterns)
//! ⡀ ⡁ ⡂ ⡃ ⡄ ⡅ ⡆ ⡇ ⡈ ⡉ ⡊ ⡋ ⡌ ⡍ ⡎ ⡏
//! ⡐ ⡑ ⡒ ⡓ ⡔ ⡕ ⡖ ⡗ ⡘ ⡙ ⡚ ⡛ ⡜ ⡝ ⡞ ⡟
//! ⡠ ⡡ ⡢ ⡣ ⡤ ⡥ ⡦ ⡧ ⡨ ⡩ ⡪ ⡫ ⡬ ⡭ ⡮ ⡯
//! ⡰ ⡱ ⡲ ⡳ ⡴ ⡵ ⡶ ⡷ ⡸ ⡹ ⡺ ⡻ ⡼ ⡽ ⡾ ⡿
//! ⢀ ⢁ ⢂ ⢃ ⢄ ⢅ ⢆ ⢇ ⢈ ⢉ ⢊ ⢋ ⢌ ⢍ ⢎ ⢏
//! ⢐ ⢑ ⢒ ⢓ ⢔ ⢕ ⢖ ⢗ ⢘ ⢙ ⢚ ⢛ ⢜ ⢝ ⢞ ⢟
//! ⢠ ⢡ ⢢ ⢣ ⢤ ⢥ ⢦ ⢧ ⢨ ⢩ ⢪ ⢫ ⢬ ⢭ ⢮ ⢯
//! ⢰ ⢱ ⢲ ⢳ ⢴ ⢵ ⢶ ⢷ ⢸ ⢹ ⢺ ⢻ ⢼ ⢽ ⢾ ⢿
//! ⣀ ⣁ ⣂ ⣃ ⣄ ⣅ ⣆ ⣇ ⣈ ⣉ ⣊ ⣋ ⣌ ⣍ ⣎ ⣏
//! ⣐ ⣑ ⣒ ⣓ ⣔ ⣕ ⣖ ⣗ ⣘ ⣙ ⣚ ⣛ ⣜ ⣝ ⣞ ⣟
//! ⣠ ⣡ ⣢ ⣣ ⣤ ⣥ ⣦ ⣧ ⣨ ⣩ ⣪ ⣫ ⣬ ⣭ ⣮ ⣯
//! ⣰ ⣱ ⣲ ⣳ ⣴ ⣵ ⣶ ⣷ ⣸ ⣹ ⣺ ⣻ ⣼ ⣽ ⣾ ⣿
//!

use crate::font::metrics::Metrics;
use crate::font::sprite::canvas::{Canvas, Color};
use crate::font::sprite::face::DrawFnError;

/// A braille pattern.
///
/// Mnemonic:
/// [t]op    - .       .
/// [u]pper  - .       .
/// [l]ower  - .       .
/// [b]ottom - .       .
///            |       |
///           [l]eft, [r]ight
///
/// Struct layout matches bit patterns of unicode codepoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Pattern {
    tl: bool,
    ul: bool,
    ll: bool,
    tr: bool,
    ur: bool,
    lr: bool,
    bl: bool,
    br: bool,
}

impl Pattern {
    fn from(cp: u32) -> Pattern {
        let b = cp as u8;
        let bit = |i: u8| (b >> i) & 1 == 1;
        Pattern {
            tl: bit(0),
            ul: bit(1),
            ll: bit(2),
            tr: bit(3),
            ur: bit(4),
            lr: bit(5),
            bl: bit(6),
            br: bit(7),
        }
    }
}

pub fn draw2800_28ff(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = metrics;

    let mut w: i32 = (width / 4).min(height / 8) as i32;
    let mut x_spacing: i32 = (width / 4) as i32;
    let mut y_spacing: i32 = (height / 8) as i32;
    let mut x_margin: i32 = x_spacing.div_euclid(2);
    let mut y_margin: i32 = y_spacing.div_euclid(2);

    let mut x_px_left: i32 = width as i32 - 2 * x_margin - x_spacing - 2 * w;

    let mut y_px_left: i32 = height as i32 - 2 * y_margin - 3 * y_spacing - 4 * w;

    // First, try hard to ensure the DOT width is non-zero
    if x_px_left >= 2 && y_px_left >= 4 && w == 0 {
        w += 1;
        x_px_left -= 2;
        y_px_left -= 4;
    }

    // Second, prefer a non-zero margin
    if x_px_left >= 2 && x_margin == 0 {
        x_margin = 1;
        x_px_left -= 2;
    }
    if y_px_left >= 2 && y_margin == 0 {
        y_margin = 1;
        y_px_left -= 2;
    }

    // Third, increase spacing
    if x_px_left >= 1 {
        x_spacing += 1;
        x_px_left -= 1;
    }
    if y_px_left >= 3 {
        y_spacing += 1;
        y_px_left -= 3;
    }

    // Fourth, margins (“spacing”, but on the sides)
    if x_px_left >= 2 {
        x_margin += 1;
        x_px_left -= 2;
    }
    if y_px_left >= 2 {
        y_margin += 1;
        y_px_left -= 2;
    }

    // Last - increase dot width
    if x_px_left >= 2 && y_px_left >= 4 {
        w += 1;
        x_px_left -= 2;
        y_px_left -= 4;
    }

    debug_assert!(x_px_left <= 1 || y_px_left <= 1);
    debug_assert!(2 * x_margin + 2 * w + x_spacing <= width as i32);
    debug_assert!(2 * y_margin + 4 * w + 3 * y_spacing <= height as i32);

    let x: [i32; 2] = [x_margin, x_margin + w + x_spacing];
    let y: [i32; 4] = {
        let mut y = [0i32; 4];
        y[0] = y_margin;
        y[1] = y[0] + w + y_spacing;
        y[2] = y[1] + w + y_spacing;
        y[3] = y[2] + w + y_spacing;
        y
    };

    debug_assert!(cp >= 0x2800);
    debug_assert!(cp <= 0x28ff);
    let p = Pattern::from(cp);

    if p.tl {
        canvas.r#box(x[0], y[0], x[0] + w, y[0] + w, Color::ON);
    }
    if p.ul {
        canvas.r#box(x[0], y[1], x[0] + w, y[1] + w, Color::ON);
    }
    if p.ll {
        canvas.r#box(x[0], y[2], x[0] + w, y[2] + w, Color::ON);
    }
    if p.bl {
        canvas.r#box(x[0], y[3], x[0] + w, y[3] + w, Color::ON);
    }
    if p.tr {
        canvas.r#box(x[1], y[0], x[1] + w, y[0] + w, Color::ON);
    }
    if p.ur {
        canvas.r#box(x[1], y[1], x[1] + w, y[1] + w, Color::ON);
    }
    if p.lr {
        canvas.r#box(x[1], y[2], x[1] + w, y[2] + w, Color::ON);
    }
    if p.br {
        canvas.r#box(x[1], y[3], x[1] + w, y[3] + w, Color::ON);
    }
    Ok(())
}
