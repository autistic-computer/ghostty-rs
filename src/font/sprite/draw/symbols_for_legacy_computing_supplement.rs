//! Port of `src/font/sprite/draw/symbols_for_legacy_computing_supplement.zig`.
//!
//! STATUS: ported
//!
//! Symbols for Legacy Computing Supplement | U+1CC00...U+1CEBF
//! https://en.wikipedia.org/wiki/Symbols_for_Legacy_Computing_Supplement
//!
//! 𜰀 𜰁 𜰂 𜰃 𜰄 𜰅 𜰆 𜰇 𜰈 𜰉 𜰊 𜰋 𜰌 𜰍 𜰎 𜰏
//! 𜰐 𜰑 𜰒 𜰓 𜰔 𜰕 𜰖 𜰗 𜰘 𜰙 𜰚 𜰛 𜰜 𜰝 𜰞 𜰟
//! 𜰠 𜰡 𜰢 𜰣 𜰤 𜰥 𜰦 𜰧 𜰨 𜰩 𜰪 𜰫 𜰬 𜰭 𜰮 𜰯
//! 𜰰 𜰱 𜰲 𜰳 𜰴 𜰵 𜰶 𜰷 𜰸 𜰹 𜰺 𜰻 𜰼 𜰽 𜰾 𜰿
//! 𜱀 𜱁 𜱂 𜱃 𜱄 𜱅 𜱆 𜱇 𜱈 𜱉 𜱊 𜱋 𜱌 𜱍 𜱎 𜱏
//! 𜱐 𜱑 𜱒 𜱓 𜱔 𜱕 𜱖 𜱗 𜱘 𜱙 𜱚 𜱛 𜱜 𜱝 𜱞 𜱟
//! 𜱠 𜱡 𜱢 𜱣 𜱤 𜱥 𜱦 𜱧 𜱨 𜱩 𜱪 𜱫 𜱬 𜱭 𜱮 𜱯
//! 𜱰 𜱱 𜱲 𜱳 𜱴 𜱵 𜱶 𜱷 𜱸 𜱹 𜱺 𜱻 𜱼 𜱽 𜱾 𜱿
//! 𜲀 𜲁 𜲂 𜲃 𜲄 𜲅 𜲆 𜲇 𜲈 𜲉 𜲊 𜲋 𜲌 𜲍 𜲎 𜲏
//! 𜲐 𜲑 𜲒 𜲓 𜲔 𜲕 𜲖 𜲗 𜲘 𜲙 𜲚 𜲛 𜲜 𜲝 𜲞 𜲟
//! 𜲠 𜲡 𜲢 𜲣 𜲤 𜲥 𜲦 𜲧 𜲨 𜲩 𜲪 𜲫 𜲬 𜲭 𜲮 𜲯
//! 𜲰 𜲱 𜲲 𜲳 𜲴 𜲵 𜲶 𜲷 𜲸 𜲹 𜲺 𜲻 𜲼 𜲽 𜲾 𜲿
//! 𜳀 𜳁 𜳂 𜳃 𜳄 𜳅 𜳆 𜳇 𜳈 𜳉 𜳊 𜳋 𜳌 𜳍 𜳎 𜳏
//! 𜳐 𜳑 𜳒 𜳓 𜳔 𜳕 𜳖 𜳗 𜳘 𜳙 𜳚 𜳛 𜳜 𜳝 𜳞 𜳟
//! 𜳠 𜳡 𜳢 𜳣 𜳤 𜳥 𜳦 𜳧 𜳨 𜳩 𜳪 𜳫 𜳬 𜳭 𜳮 𜳯
//! 𜳰 𜳱 𜳲 𜳳 𜳴 𜳵 𜳶 𜳷 𜳸 𜳹
//! 𜴀 𜴁 𜴂 𜴃 𜴄 𜴅 𜴆 𜴇 𜴈 𜴉 𜴊 𜴋 𜴌 𜴍 𜴎 𜴏
//! 𜴐 𜴑 𜴒 𜴓 𜴔 𜴕 𜴖 𜴗 𜴘 𜴙 𜴚 𜴛 𜴜 𜴝 𜴞 𜴟
//! 𜴠 𜴡 𜴢 𜴣 𜴤 𜴥 𜴦 𜴧 𜴨 𜴩 𜴪 𜴫 𜴬 𜴭 𜴮 𜴯
//! 𜴰 𜴱 𜴲 𜴳 𜴴 𜴵 𜴶 𜴷 𜴸 𜴹 𜴺 𜴻 𜴼 𜴽 𜴾 𜴿
//! 𜵀 𜵁 𜵂 𜵃 𜵄 𜵅 𜵆 𜵇 𜵈 𜵉 𜵊 𜵋 𜵌 𜵍 𜵎 𜵏
//! 𜵐 𜵑 𜵒 𜵓 𜵔 𜵕 𜵖 𜵗 𜵘 𜵙 𜵚 𜵛 𜵜 𜵝 𜵞 𜵟
//! 𜵠 𜵡 𜵢 𜵣 𜵤 𜵥 𜵦 𜵧 𜵨 𜵩 𜵪 𜵫 𜵬 𜵭 𜵮 𜵯
//! 𜵰 𜵱 𜵲 𜵳 𜵴 𜵵 𜵶 𜵷 𜵸 𜵹 𜵺 𜵻 𜵼 𜵽 𜵾 𜵿
//! 𜶀 𜶁 𜶂 𜶃 𜶄 𜶅 𜶆 𜶇 𜶈 𜶉 𜶊 𜶋 𜶌 𜶍 𜶎 𜶏
//! 𜶐 𜶑 𜶒 𜶓 𜶔 𜶕 𜶖 𜶗 𜶘 𜶙 𜶚 𜶛 𜶜 𜶝 𜶞 𜶟
//! 𜶠 𜶡 𜶢 𜶣 𜶤 𜶥 𜶦 𜶧 𜶨 𜶩 𜶪 𜶫 𜶬 𜶭 𜶮 𜶯
//! 𜶰 𜶱 𜶲 𜶳 𜶴 𜶵 𜶶 𜶷 𜶸 𜶹 𜶺 𜶻 𜶼 𜶽 𜶾 𜶿
//! 𜷀 𜷁 𜷂 𜷃 𜷄 𜷅 𜷆 𜷇 𜷈 𜷉 𜷊 𜷋 𜷌 𜷍 𜷎 𜷏
//! 𜷐 𜷑 𜷒 𜷓 𜷔 𜷕 𜷖 𜷗 𜷘 𜷙 𜷚 𜷛 𜷜 𜷝 𜷞 𜷟
//! 𜷠 𜷡 𜷢 𜷣 𜷤 𜷥 𜷦 𜷧 𜷨 𜷩 𜷪 𜷫 𜷬 𜷭 𜷮 𜷯
//! 𜷰 𜷱 𜷲 𜷳 𜷴 𜷵 𜷶 𜷷 𜷸 𜷹 𜷺 𜷻 𜷼 𜷽 𜷾 𜷿
//! 𜸀 𜸁 𜸂 𜸃 𜸄 𜸅 𜸆 𜸇 𜸈 𜸉 𜸊 𜸋 𜸌 𜸍 𜸎 𜸏
//! 𜸐 𜸑 𜸒 𜸓 𜸔 𜸕 𜸖 𜸗 𜸘 𜸙 𜸚 𜸛 𜸜 𜸝 𜸞 𜸟
//! 𜸠 𜸡 𜸢 𜸣 𜸤 𜸥 𜸦 𜸧 𜸨 𜸩 𜸪 𜸫 𜸬 𜸭 𜸮 𜸯
//! 𜸰 𜸱 𜸲 𜸳 𜸴 𜸵 𜸶 𜸷 𜸸 𜸹 𜸺 𜸻 𜸼 𜸽 𜸾 𜸿
//! 𜹀 𜹁 𜹂 𜹃 𜹄 𜹅 𜹆 𜹇 𜹈 𜹉 𜹊 𜹋 𜹌 𜹍 𜹎 𜹏
//! 𜹐 𜹑 𜹒 𜹓 𜹔 𜹕 𜹖 𜹗 𜹘 𜹙 𜹚 𜹛 𜹜 𜹝 𜹞 𜹟
//! 𜹠 𜹡 𜹢 𜹣 𜹤 𜹥 𜹦 𜹧 𜹨 𜹩 𜹪 𜹫 𜹬 𜹭 𜹮 𜹯
//! 𜹰 𜹱 𜹲 𜹳 𜹴 𜹵 𜹶 𜹷 𜹸 𜹹 𜹺 𜹻 𜹼 𜹽 𜹾 𜹿
//! 𜺀 𜺁 𜺂 𜺃 𜺄 𜺅 𜺆 𜺇 𜺈 𜺉 𜺊 𜺋 𜺌 𜺍 𜺎 𜺏
//! 𜺐 𜺑 𜺒 𜺓 𜺔 𜺕 𜺖 𜺗 𜺘 𜺙 𜺚 𜺛 𜺜 𜺝 𜺞 𜺟
//! 𜺠 𜺡 𜺢 𜺣 𜺤 𜺥 𜺦 𜺧 𜺨 𜺩 𜺪 𜺫 𜺬 𜺭 𜺮 𜺯
//! 𜺰 𜺱 𜺲 𜺳
//!

use super::common::{fill, Alignment, Corner, Fraction};
use super::r#box::{self, Lines, Style};
use super::symbols_for_legacy_computing as sflc;
use crate::font::metrics::Metrics;
use crate::font::sprite::canvas::{CapMode, Canvas, Color, StrokeOptions};
use crate::font::sprite::face::DrawFnError;

const OCTANT_MIN: u32 = 0x1cd00;
const OCTANT_MAX: u32 = 0x1cde5;

/// Octant representation. We use the funny numeric string keys
/// so its easier to parse the actual name used in the Symbols for
/// Legacy Computing spec.
///
/// (Zig `packed struct(u8)` with fields `"1"`..`"8"`; bit `n - 1` is
/// octant `n`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Octant(u8);

impl Octant {
    #[inline]
    fn has(self, n: u8) -> bool {
        (self.0 >> (n - 1)) & 1 == 1
    }
}

const OCTANTS_LEN: usize = (OCTANT_MAX - OCTANT_MIN + 1) as usize;

/// Parse the octant data. This is all done at compile time so
/// that this is static data that is embedded in the binary.
const OCTANTS: [Octant; OCTANTS_LEN] = parse_octants(include_bytes!("octants.txt"));

const fn parse_octants(data: &[u8]) -> [Octant; OCTANTS_LEN] {
    let mut result = [Octant(0); OCTANTS_LEN];
    let mut i: usize = 0;

    let mut pos: usize = 0;
    while pos < data.len() {
        // Find the end of this line.
        let mut end = pos;
        while end < data.len() && data[end] != b'\n' {
            end += 1;
        }
        // Trim \r so this works with both LF and CRLF line endings,
        // since git may convert octants.txt to CRLF on Windows checkouts.
        let mut line_end = end;
        if line_end > pos && data[line_end - 1] == b'\r' {
            line_end -= 1;
        }

        // Skip comments
        if !(line_end == pos || data[pos] == b'#') {
            // Octants are in the format "BLOCK OCTANT-1235". The numbers
            // at the end are keys into our packed struct.
            let mut idx = pos;
            while data[idx] != b'-' {
                idx += 1;
            }
            let mut bits: u8 = 0;
            let mut c = idx + 1;
            while c < line_end {
                let n = data[c] - b'0';
                assert!(n >= 1 && n <= 8);
                bits |= 1 << (n - 1);
                c += 1;
            }
            result[i] = Octant(bits);
            i += 1;
        }

        pos = end + 1;
    }

    assert!(i == OCTANTS_LEN);
    result
}

/// Octants
pub fn draw1cd00_1cde5(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = width;
    let _ = height;

    let oct = OCTANTS[(cp - OCTANT_MIN) as usize];
    if oct.has(1) {
        fill(metrics, canvas, Fraction::Zero, Fraction::Half, Fraction::Zero, Fraction::OneQuarter);
    }
    if oct.has(2) {
        fill(metrics, canvas, Fraction::Half, Fraction::Full, Fraction::Zero, Fraction::OneQuarter);
    }
    if oct.has(3) {
        fill(metrics, canvas, Fraction::Zero, Fraction::Half, Fraction::OneQuarter, Fraction::TwoQuarters);
    }
    if oct.has(4) {
        fill(metrics, canvas, Fraction::Half, Fraction::Full, Fraction::OneQuarter, Fraction::TwoQuarters);
    }
    if oct.has(5) {
        fill(metrics, canvas, Fraction::Zero, Fraction::Half, Fraction::TwoQuarters, Fraction::ThreeQuarters);
    }
    if oct.has(6) {
        fill(metrics, canvas, Fraction::Half, Fraction::Full, Fraction::TwoQuarters, Fraction::ThreeQuarters);
    }
    if oct.has(7) {
        fill(metrics, canvas, Fraction::Zero, Fraction::Half, Fraction::ThreeQuarters, Fraction::End);
    }
    if oct.has(8) {
        fill(metrics, canvas, Fraction::Half, Fraction::Full, Fraction::ThreeQuarters, Fraction::End);
    }
    Ok(())
}

// Separated Block Quadrants
// 𜰡 𜰢 𜰣 𜰤 𜰥 𜰦 𜰧 𜰨 𜰩 𜰪 𜰫 𜰬 𜰭 𜰮 𜰯
pub fn draw1cc21_1cc2f(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = metrics;

    // Struct laid out to match the codepoint order so we can cast from it.
    // (Zig `packed struct(u4) { tl, tr, bl, br }`.)
    let quad = ((cp - 0x1CC20) & 0xF) as u8;
    let (tl, tr, bl, br) = (quad & 1 != 0, quad & 2 != 0, quad & 4 != 0, quad & 8 != 0);

    let gap: i32 = (width / 12).max(1) as i32;

    let mid_gap_x: i32 = gap * 2 + (width % 2) as i32;
    let mid_gap_y: i32 = gap * 2 + (height % 2) as i32;

    let w: i32 = (width as i32 - gap * 2 - mid_gap_x) / 2;
    debug_assert!((width as i32 - gap * 2 - mid_gap_x) % 2 == 0);
    let h: i32 = (height as i32 - gap * 2 - mid_gap_y) / 2;
    debug_assert!((height as i32 - gap * 2 - mid_gap_y) % 2 == 0);

    if tl {
        canvas.r#box(gap, gap, gap + w, gap + h, Color::ON);
    }
    if tr {
        canvas.r#box(
            gap + w + mid_gap_x,
            gap,
            gap + w + mid_gap_x + w,
            gap + h,
            Color::ON,
        );
    }
    if bl {
        canvas.r#box(
            gap,
            gap + h + mid_gap_y,
            gap + w,
            gap + h + mid_gap_y + h,
            Color::ON,
        );
    }
    if br {
        canvas.r#box(
            gap + w + mid_gap_x,
            gap + h + mid_gap_y,
            gap + w + mid_gap_x + w,
            gap + h + mid_gap_y + h,
            Color::ON,
        );
    }
    Ok(())
}


/// Twelfth and Quarter circle pieces.
/// 𜰰 𜰱 𜰲 𜰳 𜰴 𜰵 𜰶 𜰷 𜰸 𜰹 𜰺 𜰻 𜰼 𜰽 𜰾 𜰿
///
/// 𜰰𜰱𜰲𜰳
/// 𜰴𜰵𜰶𜰷
/// 𜰸𜰹𜰺𜰻
/// 𜰼𜰽𜰾𜰿
///
/// These are actually ellipses, sized to touch
/// the edge of their enclosing set of cells.
pub fn draw1cc30_1cc3f(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    match cp {
        // 𜰰 UPPER LEFT TWELFTH CIRCLE
        0x1CC30 => circle_piece(canvas, width, height, metrics, 0.0, 0.0, 2.0, 2.0, Corner::Tl)?,
        // 𜰱 UPPER CENTRE LEFT TWELFTH CIRCLE
        0x1CC31 => circle_piece(canvas, width, height, metrics, 1.0, 0.0, 2.0, 2.0, Corner::Tl)?,
        // 𜰲 UPPER CENTRE RIGHT TWELFTH CIRCLE
        0x1CC32 => circle_piece(canvas, width, height, metrics, 2.0, 0.0, 2.0, 2.0, Corner::Tr)?,
        // 𜰳 UPPER RIGHT TWELFTH CIRCLE
        0x1CC33 => circle_piece(canvas, width, height, metrics, 3.0, 0.0, 2.0, 2.0, Corner::Tr)?,
        // 𜰴 UPPER MIDDLE LEFT TWELFTH CIRCLE
        0x1CC34 => circle_piece(canvas, width, height, metrics, 0.0, 1.0, 2.0, 2.0, Corner::Tl)?,
        // 𜰵 UPPER LEFT QUARTER CIRCLE
        0x1CC35 => circle_piece(canvas, width, height, metrics, 0.0, 0.0, 1.0, 1.0, Corner::Tl)?,
        // 𜰶 UPPER RIGHT QUARTER CIRCLE
        0x1CC36 => circle_piece(canvas, width, height, metrics, 1.0, 0.0, 1.0, 1.0, Corner::Tr)?,
        // 𜰷 UPPER MIDDLE RIGHT TWELFTH CIRCLE
        0x1CC37 => circle_piece(canvas, width, height, metrics, 3.0, 1.0, 2.0, 2.0, Corner::Tr)?,
        // 𜰸 LOWER MIDDLE LEFT TWELFTH CIRCLE
        0x1CC38 => circle_piece(canvas, width, height, metrics, 0.0, 2.0, 2.0, 2.0, Corner::Bl)?,
        // 𜰹 LOWER LEFT QUARTER CIRCLE
        0x1CC39 => circle_piece(canvas, width, height, metrics, 0.0, 1.0, 1.0, 1.0, Corner::Bl)?,
        // 𜰺 LOWER RIGHT QUARTER CIRCLE
        0x1CC3A => circle_piece(canvas, width, height, metrics, 1.0, 1.0, 1.0, 1.0, Corner::Br)?,
        // 𜰻 LOWER MIDDLE RIGHT TWELFTH CIRCLE
        0x1CC3B => circle_piece(canvas, width, height, metrics, 3.0, 2.0, 2.0, 2.0, Corner::Br)?,
        // 𜰼 LOWER LEFT TWELFTH CIRCLE
        0x1CC3C => circle_piece(canvas, width, height, metrics, 0.0, 3.0, 2.0, 2.0, Corner::Bl)?,
        // 𜰽 LOWER CENTRE LEFT TWELFTH CIRCLE
        0x1CC3D => circle_piece(canvas, width, height, metrics, 1.0, 3.0, 2.0, 2.0, Corner::Bl)?,
        // 𜰾 LOWER CENTRE RIGHT TWELFTH CIRCLE
        0x1CC3E => circle_piece(canvas, width, height, metrics, 2.0, 3.0, 2.0, 2.0, Corner::Br)?,
        // 𜰿 LOWER RIGHT TWELFTH CIRCLE
        0x1CC3F => circle_piece(canvas, width, height, metrics, 3.0, 3.0, 2.0, 2.0, Corner::Br)?,
        _ => unreachable!(),
    }
    Ok(())
}

/// TODO: These two characters should be easy, but it's not clear how they're
///       meant to align with adjacent cells, what characters they're meant to
///       be used with:
///       - 1CC1F 𜰟 BOX DRAWINGS DOUBLE DIAGONAL UPPER RIGHT TO LOWER LEFT
///       - 1CC20 𜰠 BOX DRAWINGS DOUBLE DIAGONAL UPPER LEFT TO LOWER RIGHT
pub fn draw1cc1b_1cc1e(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let w: i32 = width as i32;
    let h: i32 = height as i32;
    let t: i32 = metrics.box_thickness as i32;
    match cp {
        // 𜰛 BOX DRAWINGS LIGHT HORIZONTAL AND UPPER RIGHT
        0x1CC1B => {
            r#box::lines_char(metrics, canvas, Lines { left: Style::Light, right: Style::Light, ..Lines::NONE });
            canvas.r#box(w - t, 0, w, h.div_euclid(2), Color::ON);
        },
        // 𜰜 BOX DRAWINGS LIGHT HORIZONTAL AND LOWER RIGHT
        0x1CC1C => {
            r#box::lines_char(metrics, canvas, Lines { left: Style::Light, right: Style::Light, ..Lines::NONE });
            canvas.r#box(w - t, h.div_euclid(2), w, h, Color::ON);
        },
        // 𜰝 BOX DRAWINGS LIGHT TOP AND UPPER LEFT
        0x1CC1D => {
            canvas.r#box(0, 0, w, t, Color::ON);
            canvas.r#box(0, 0, t, h.div_euclid(2), Color::ON);
        },
        // 𜰞 BOX DRAWINGS LIGHT BOTTOM AND LOWER LEFT
        0x1CC1E => {
            canvas.r#box(0, h - t, w, h, Color::ON);
            canvas.r#box(0, h.div_euclid(2), t, h, Color::ON);
        },
        _ => unreachable!(),
    }
    Ok(())
}

/// 𜸀 RIGHT HALF AND LEFT HALF WHITE CIRCLE
pub fn draw1ce00(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = cp;
    let _ = width;
    let _ = height;
    sflc::circle(metrics, canvas, Alignment::LEFT, false);
    sflc::circle(metrics, canvas, Alignment::RIGHT, false);
    Ok(())
}

/// 𜸁 LOWER HALF AND UPPER HALF WHITE CIRCLE
pub fn draw1ce01(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = cp;
    let _ = width;
    let _ = height;
    sflc::circle(metrics, canvas, Alignment::TOP, false);
    sflc::circle(metrics, canvas, Alignment::BOTTOM, false);
    Ok(())
}

/// 𜸋 LEFT HALF WHITE ELLIPSE
pub fn draw1ce0b(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = cp;
    circle_piece(canvas, width, height, metrics, 0.0, 0.0, 1.0, 0.5, Corner::Tl)?;
    circle_piece(canvas, width, height, metrics, 0.0, 0.0, 1.0, 0.5, Corner::Bl)?;
    Ok(())
}

/// 𜸌 RIGHT HALF WHITE ELLIPSE
pub fn draw1ce0c(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = cp;
    circle_piece(canvas, width, height, metrics, 1.0, 0.0, 1.0, 0.5, Corner::Tr)?;
    circle_piece(canvas, width, height, metrics, 1.0, 0.0, 1.0, 0.5, Corner::Br)?;
    Ok(())
}

pub fn draw1ce16_1ce19(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let w: i32 = width as i32;
    let h: i32 = height as i32;
    let t: i32 = metrics.box_thickness as i32;
    match cp {
        // 𜸖 BOX DRAWINGS LIGHT VERTICAL AND TOP RIGHT
        0x1CE16 => {
            r#box::lines_char(metrics, canvas, Lines { up: Style::Light, down: Style::Light, ..Lines::NONE });
            canvas.r#box(w.div_euclid(2), 0, w, t, Color::ON);
        },
        // 𜸗 BOX DRAWINGS LIGHT VERTICAL AND BOTTOM RIGHT
        0x1CE17 => {
            r#box::lines_char(metrics, canvas, Lines { up: Style::Light, down: Style::Light, ..Lines::NONE });
            canvas.r#box(w.div_euclid(2), h - t, w, h, Color::ON);
        },
        // 𜸘 BOX DRAWINGS LIGHT VERTICAL AND TOP LEFT
        0x1CE18 => {
            r#box::lines_char(metrics, canvas, Lines { up: Style::Light, down: Style::Light, ..Lines::NONE });
            canvas.r#box(0, 0, w.div_euclid(2), t, Color::ON);
        },
        // 𜸙 BOX DRAWINGS LIGHT VERTICAL AND BOTTOM LEFT
        0x1CE19 => {
            r#box::lines_char(metrics, canvas, Lines { up: Style::Light, down: Style::Light, ..Lines::NONE });
            canvas.r#box(0, h - t, w.div_euclid(2), h, Color::ON);
        },
        _ => unreachable!(),
    }
    Ok(())
}

/// Separated Block Sextants
pub fn draw1ce51_1ce8f(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = metrics;

    // Struct laid out to match the codepoint order so we can cast from it.
    // (Zig `packed struct(u6) { tl, tr, ml, mr, bl, br }`.)
    let sex = ((cp - 0x1CE50) & 0x3F) as u8;
    let bit = |i: u8| (sex >> i) & 1 == 1;
    let (tl, tr, ml, mr, bl, br) = (bit(0), bit(1), bit(2), bit(3), bit(4), bit(5));

    let gap: i32 = (width / 12).max(1) as i32;

    let mid_gap_x: i32 = gap * 2 + (width % 2) as i32;
    let y_extra: i32 = (height % 3) as i32;
    let mid_gap_y: i32 = gap * 2 + y_extra.div_euclid(2);

    let w: i32 = (width as i32 - gap * 2 - mid_gap_x) / 2;
    debug_assert!((width as i32 - gap * 2 - mid_gap_x) % 2 == 0);
    let h: i32 = (height as i32 - gap * 2 - mid_gap_y * 2).div_euclid(3);
    // Distribute any leftover height in to the middle row of blocks.
    let h_m: i32 = height as i32 - gap * 2 - mid_gap_y * 2 - h * 2;

    if tl {
        canvas.r#box(gap, gap, gap + w, gap + h, Color::ON);
    }
    if tr {
        canvas.r#box(
            gap + w + mid_gap_x,
            gap,
            gap + w + mid_gap_x + w,
            gap + h,
            Color::ON,
        );
    }
    if ml {
        canvas.r#box(
            gap,
            gap + h + mid_gap_y,
            gap + w,
            gap + h + mid_gap_y + h_m,
            Color::ON,
        );
    }
    if mr {
        canvas.r#box(
            gap + w + mid_gap_x,
            gap + h + mid_gap_y,
            gap + w + mid_gap_x + w,
            gap + h + mid_gap_y + h_m,
            Color::ON,
        );
    }
    if bl {
        canvas.r#box(
            gap,
            gap + h + mid_gap_y + h_m + mid_gap_y,
            gap + w,
            gap + h + mid_gap_y + h_m + mid_gap_y + h,
            Color::ON,
        );
    }
    if br {
        canvas.r#box(
            gap + w + mid_gap_x,
            gap + h + mid_gap_y + h_m + mid_gap_y,
            gap + w + mid_gap_x + w,
            gap + h + mid_gap_y + h_m + mid_gap_y + h,
            Color::ON,
        );
    }
    Ok(())
}

/// Sixteenth Blocks
pub fn draw1ce90_1ceaf(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError> {
    let _ = width;
    let _ = height;
    let q = Fraction::QUARTERS;
    match cp {
        // 𜺐 UPPER LEFT ONE SIXTEENTH BLOCK
        0x1CE90 => fill(metrics, canvas, q[0], q[1], q[0], q[1]),
        // 𜺑 UPPER CENTRE LEFT ONE SIXTEENTH BLOCK
        0x1CE91 => fill(metrics, canvas, q[1], q[2], q[0], q[1]),
        // 𜺒 UPPER CENTRE RIGHT ONE SIXTEENTH BLOCK
        0x1CE92 => fill(metrics, canvas, q[2], q[3], q[0], q[1]),
        // 𜺓 UPPER RIGHT ONE SIXTEENTH BLOCK
        0x1CE93 => fill(metrics, canvas, q[3], q[4], q[0], q[1]),
        // 𜺔 UPPER MIDDLE LEFT ONE SIXTEENTH BLOCK
        0x1CE94 => fill(metrics, canvas, q[0], q[1], q[1], q[2]),
        // 𜺕 UPPER MIDDLE CENTRE LEFT ONE SIXTEENTH BLOCK
        0x1CE95 => fill(metrics, canvas, q[1], q[2], q[1], q[2]),
        // 𜺖 UPPER MIDDLE CENTRE RIGHT ONE SIXTEENTH BLOCK
        0x1CE96 => fill(metrics, canvas, q[2], q[3], q[1], q[2]),
        // 𜺗 UPPER MIDDLE RIGHT ONE SIXTEENTH BLOCK
        0x1CE97 => fill(metrics, canvas, q[3], q[4], q[1], q[2]),
        // 𜺘 LOWER MIDDLE LEFT ONE SIXTEENTH BLOCK
        0x1CE98 => fill(metrics, canvas, q[0], q[1], q[2], q[3]),
        // 𜺙 LOWER MIDDLE CENTRE LEFT ONE SIXTEENTH BLOCK
        0x1CE99 => fill(metrics, canvas, q[1], q[2], q[2], q[3]),
        // 𜺚 LOWER MIDDLE CENTRE RIGHT ONE SIXTEENTH BLOCK
        0x1CE9A => fill(metrics, canvas, q[2], q[3], q[2], q[3]),
        // 𜺛 LOWER MIDDLE RIGHT ONE SIXTEENTH BLOCK
        0x1CE9B => fill(metrics, canvas, q[3], q[4], q[2], q[3]),
        // 𜺜 LOWER LEFT ONE SIXTEENTH BLOCK
        0x1CE9C => fill(metrics, canvas, q[0], q[1], q[3], q[4]),
        // 𜺝 LOWER CENTRE LEFT ONE SIXTEENTH BLOCK
        0x1CE9D => fill(metrics, canvas, q[1], q[2], q[3], q[4]),
        // 𜺞 LOWER CENTRE RIGHT ONE SIXTEENTH BLOCK
        0x1CE9E => fill(metrics, canvas, q[2], q[3], q[3], q[4]),
        // 𜺟 LOWER RIGHT ONE SIXTEENTH BLOCK
        0x1CE9F => fill(metrics, canvas, q[3], q[4], q[3], q[4]),

        // 𜺠 RIGHT HALF LOWER ONE QUARTER BLOCK
        0x1CEA0 => fill(metrics, canvas, q[2], q[4], q[3], q[4]),
        // 𜺡 RIGHT THREE QUARTERS LOWER ONE QUARTER BLOCK
        0x1CEA1 => fill(metrics, canvas, q[1], q[4], q[3], q[4]),
        // 𜺢 LEFT THREE QUARTERS LOWER ONE QUARTER BLOCK
        0x1CEA2 => fill(metrics, canvas, q[0], q[3], q[3], q[4]),
        // 𜺣 LEFT HALF LOWER ONE QUARTER BLOCK
        0x1CEA3 => fill(metrics, canvas, q[0], q[2], q[3], q[4]),
        // 𜺤 LOWER HALF LEFT ONE QUARTER BLOCK
        0x1CEA4 => fill(metrics, canvas, q[0], q[1], q[2], q[4]),
        // 𜺥 LOWER THREE QUARTERS LEFT ONE QUARTER BLOCK
        0x1CEA5 => fill(metrics, canvas, q[0], q[1], q[1], q[4]),
        // 𜺦 UPPER THREE QUARTERS LEFT ONE QUARTER BLOCK
        0x1CEA6 => fill(metrics, canvas, q[0], q[1], q[0], q[3]),
        // 𜺧 UPPER HALF LEFT ONE QUARTER BLOCK
        0x1CEA7 => fill(metrics, canvas, q[0], q[1], q[0], q[2]),
        // 𜺨 LEFT HALF UPPER ONE QUARTER BLOCK
        0x1CEA8 => fill(metrics, canvas, q[0], q[2], q[0], q[1]),
        // 𜺩 LEFT THREE QUARTERS UPPER ONE QUARTER BLOCK
        0x1CEA9 => fill(metrics, canvas, q[0], q[3], q[0], q[1]),
        // 𜺪 RIGHT THREE QUARTERS UPPER ONE QUARTER BLOCK
        0x1CEAA => fill(metrics, canvas, q[1], q[4], q[0], q[1]),
        // 𜺫 RIGHT HALF UPPER ONE QUARTER BLOCK
        0x1CEAB => fill(metrics, canvas, q[2], q[4], q[0], q[1]),
        // 𜺬 UPPER HALF RIGHT ONE QUARTER BLOCK
        0x1CEAC => fill(metrics, canvas, q[3], q[4], q[0], q[2]),
        // 𜺭 UPPER THREE QUARTERS RIGHT ONE QUARTER BLOCK
        0x1CEAD => fill(metrics, canvas, q[3], q[4], q[0], q[3]),
        // 𜺮 LOWER THREE QUARTERS RIGHT ONE QUARTER BLOCK
        0x1CEAE => fill(metrics, canvas, q[3], q[4], q[1], q[4]),
        // 𜺯 LOWER HALF RIGHT ONE QUARTER BLOCK
        0x1CEAF => fill(metrics, canvas, q[3], q[4], q[2], q[4]),

        _ => unreachable!(),
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn circle_piece(
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    corner: Corner,
) -> Result<(), DrawFnError> {
    // Radius in pixels of the arc we need to draw.
    let wdth: f64 = width as f64 * w;
    let hght: f64 = height as f64 * h;

    // Position in pixels (rather than cells) for x/y
    let xp: f64 = width as f64 * x;
    let yp: f64 = height as f64 * y;

    // Set the clip so we don't include anything outside of the cell.
    canvas.clip_left = canvas.padding_x;
    canvas.clip_right = canvas.padding_x;
    canvas.clip_top = canvas.padding_y;
    canvas.clip_bottom = canvas.padding_y;

    // Coefficient for approximating a circular arc.
    let c: f64 = (std::f64::consts::SQRT_2 - 1.0) * 4.0 / 3.0;
    let cw = c * wdth;
    let ch = c * hght;

    let thick = metrics.box_thickness as f64;
    let ht = thick * 0.5;

    let mut path = canvas.static_path(2);

    match corner {
        Corner::Tl => {
            path.move_to(wdth - xp, ht - yp);
            path.curve_to(
                wdth - cw - xp,
                ht - yp,
                ht - xp,
                hght - ch - yp,
                ht - xp,
                hght - yp,
            );
        }
        Corner::Tr => {
            path.move_to(wdth - xp, ht - yp);
            path.curve_to(
                wdth + cw - xp,
                ht - yp,
                wdth * 2.0 - ht - xp,
                hght - ch - yp,
                wdth * 2.0 - ht - xp,
                hght - yp,
            );
        }
        Corner::Bl => {
            path.move_to(ht - xp, hght - yp);
            path.curve_to(
                ht - xp,
                hght + ch - yp,
                wdth - cw - xp,
                hght * 2.0 - ht - yp,
                wdth - xp,
                hght * 2.0 - ht - yp,
            );
        }
        Corner::Br => {
            path.move_to(wdth * 2.0 - ht - xp, hght - yp);
            path.curve_to(
                wdth * 2.0 - ht - xp,
                hght + ch - yp,
                wdth + cw - xp,
                hght * 2.0 - ht - yp,
                wdth - xp,
                hght * 2.0 - ht - yp,
            );
        }
    }

    canvas.stroke_path(
        &path,
        StrokeOptions {
            line_cap_mode: CapMode::Butt,
            line_width: metrics.box_thickness as f64,
            ..StrokeOptions::default()
        },
        Color::ON,
    )
}
