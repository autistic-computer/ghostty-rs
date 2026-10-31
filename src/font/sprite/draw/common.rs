//! Port of `src/font/sprite/draw/common.zig`.
//!
//! STATUS: ported
//!
//! This file contains a set of useful helper functions
//! and types for drawing our sprite font glyphs. These
//! are generally applicable to multiple sets of glyphs
//! rather than being single-use.

use crate::font::metrics::Metrics;
use crate::font::sprite::canvas::{Canvas, Color};

// Utility names for common fractions
pub const ONE_EIGHTH: f64 = 0.125;
pub const ONE_QUARTER: f64 = 0.25;
pub const ONE_THIRD: f64 = 1.0 / 3.0;
pub const THREE_EIGHTHS: f64 = 0.375;
pub const HALF: f64 = 0.5;
pub const FIVE_EIGHTHS: f64 = 0.625;
pub const TWO_THIRDS: f64 = 2.0 / 3.0;
pub const THREE_QUARTERS: f64 = 0.75;
pub const SEVEN_EIGHTHS: f64 = 0.875;

/// The thickness of a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Thickness {
    SuperLight,
    Light,
    Heavy,
}

impl Thickness {
    /// Calculate the real height of a line based on its
    /// thickness and a base thickness value. The base
    /// thickness value is expected to be in pixels.
    pub fn height(self, base: u32) -> u32 {
        match self {
            Thickness::SuperLight => (base / 2).max(1),
            Thickness::Light => base,
            Thickness::Heavy => base * 2,
        }
    }
}

/// Shades.
///
/// (Zig non-exhaustive `enum(u8)`; any other value is allowed too.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shade(pub u8);

impl Shade {
    pub const OFF: Shade = Shade(0x00);
    pub const LIGHT: Shade = Shade(0x40);
    pub const MEDIUM: Shade = Shade(0x80);
    pub const DARK: Shade = Shade(0xc0);
    pub const ON: Shade = Shade(0xff);

    /// The canvas color with the same alpha value.
    #[inline]
    pub fn color(self) -> Color {
        Color(self.0)
    }
}

/// Applicable to any set of glyphs with features
/// that may be present or not in each quadrant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Quads {
    pub tl: bool,
    pub tr: bool,
    pub bl: bool,
    pub br: bool,
}

/// A corner of a cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Corner {
    Tl,
    Tr,
    Bl,
    Br,
}

/// An edge of a cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Top,
    Left,
    Bottom,
    Right,
}

/// Horizontal part of an [`Alignment`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HorizontalAlignment {
    Left,
    Right,
    Center,
}

/// Vertical part of an [`Alignment`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerticalAlignment {
    Top,
    Bottom,
    Middle,
}

/// Alignment of a figure within a cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Alignment {
    pub horizontal: HorizontalAlignment,
    pub vertical: VerticalAlignment,
}

impl Default for Alignment {
    fn default() -> Self {
        Alignment::CENTER
    }
}

impl Alignment {
    const fn new(horizontal: HorizontalAlignment, vertical: VerticalAlignment) -> Alignment {
        Alignment {
            horizontal,
            vertical,
        }
    }

    pub const UPPER: Alignment = Alignment::new(HorizontalAlignment::Center, VerticalAlignment::Top);
    pub const LOWER: Alignment =
        Alignment::new(HorizontalAlignment::Center, VerticalAlignment::Bottom);
    pub const LEFT: Alignment = Alignment::new(HorizontalAlignment::Left, VerticalAlignment::Middle);
    pub const RIGHT: Alignment =
        Alignment::new(HorizontalAlignment::Right, VerticalAlignment::Middle);

    pub const UPPER_LEFT: Alignment =
        Alignment::new(HorizontalAlignment::Left, VerticalAlignment::Top);
    pub const UPPER_RIGHT: Alignment =
        Alignment::new(HorizontalAlignment::Right, VerticalAlignment::Top);
    pub const LOWER_LEFT: Alignment =
        Alignment::new(HorizontalAlignment::Left, VerticalAlignment::Bottom);
    pub const LOWER_RIGHT: Alignment =
        Alignment::new(HorizontalAlignment::Right, VerticalAlignment::Bottom);

    pub const CENTER: Alignment =
        Alignment::new(HorizontalAlignment::Center, VerticalAlignment::Middle);

    pub const UPPER_CENTER: Alignment = Alignment::UPPER;
    pub const LOWER_CENTER: Alignment = Alignment::LOWER;
    pub const MIDDLE_LEFT: Alignment = Alignment::LEFT;
    pub const MIDDLE_RIGHT: Alignment = Alignment::RIGHT;
    pub const MIDDLE_CENTER: Alignment = Alignment::CENTER;

    pub const TOP: Alignment = Alignment::UPPER;
    pub const BOTTOM: Alignment = Alignment::LOWER;
    pub const CENTER_TOP: Alignment = Alignment::TOP;
    pub const CENTER_BOTTOM: Alignment = Alignment::BOTTOM;

    pub const TOP_LEFT: Alignment = Alignment::UPPER_LEFT;
    pub const TOP_RIGHT: Alignment = Alignment::UPPER_RIGHT;
    pub const BOTTOM_LEFT: Alignment = Alignment::LOWER_LEFT;
    pub const BOTTOM_RIGHT: Alignment = Alignment::LOWER_RIGHT;
}

/// A value that indicates some fraction across
/// the cell either horizontally or vertically.
///
/// This has some redundant names in it so that you can
/// use whichever one feels most semantically appropriate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fraction {
    // Names for the min edge
    Start,
    Left,
    Top,
    Zero,

    // Names based on eighths
    Eighth,
    OneEighth,
    TwoEighths,
    ThreeEighths,
    FourEighths,
    FiveEighths,
    SixEighths,
    SevenEighths,

    // Names based on quarters
    Quarter,
    OneQuarter,
    TwoQuarters,
    ThreeQuarters,

    // Names based on thirds
    Third,
    OneThird,
    TwoThirds,

    // Names based on halves
    Half,
    OneHalf,

    // Alternative names for 1/2
    Center,
    Middle,

    // Names for the max edge
    End,
    Right,
    Bottom,
    One,
    Full,
}

impl Fraction {
    /// This can be indexed to get the fraction for `i/8`.
    pub const EIGHTHS: [Fraction; 9] = [
        Fraction::Zero,
        Fraction::OneEighth,
        Fraction::TwoEighths,
        Fraction::ThreeEighths,
        Fraction::FourEighths,
        Fraction::FiveEighths,
        Fraction::SixEighths,
        Fraction::SevenEighths,
        Fraction::One,
    ];

    /// This can be indexed to get the fraction for `i/4`.
    pub const QUARTERS: [Fraction; 5] = [
        Fraction::Zero,
        Fraction::OneQuarter,
        Fraction::TwoQuarters,
        Fraction::ThreeQuarters,
        Fraction::One,
    ];

    /// This can be indexed to get the fraction for `i/3`.
    pub const THIRDS: [Fraction; 4] = [
        Fraction::Zero,
        Fraction::OneThird,
        Fraction::TwoThirds,
        Fraction::One,
    ];

    /// This can be indexed to get the fraction for `i/2`.
    pub const HALVES: [Fraction; 3] = [Fraction::Zero, Fraction::OneHalf, Fraction::One];

    /// Get the x position for this fraction across a particular
    /// size (width or height), assuming it will be used as the
    /// min (left/top) coordinate for a block.
    #[inline]
    pub fn min(self, size: u32) -> i32 {
        let s = size as f64;
        // For min coordinates, we want to align with the complementary
        // fraction taken from the end, this ensures that rounding evens
        // out, so that for example, if `size` is `7`, and we're looking
        // at the `half` line, `size - round((1 - 0.5) * size)` => `3`;
        // whereas the max coordinate directly rounds, which means that
        // both `start` -> `half` and `half` -> `end` will be 4px, from
        // `0` -> `4` and `3` -> `7`.
        (s - ((1.0 - self.fraction()) * s).round()) as i32
    }

    /// Get the x position for this fraction across a particular
    /// size (width or height), assuming it will be used as the
    /// max (right/bottom) coordinate for a block.
    #[inline]
    pub fn max(self, size: u32) -> i32 {
        let s = size as f64;
        // See explanation of why these are different in `min`.
        (self.fraction() * s).round() as i32
    }

    /// Get this fraction across a particular size (width/height).
    /// If you need an integer, use `min` or `max` instead, since
    /// they contain special logic for consistent alignment. This
    /// is for when you're drawing with paths and don't care about
    /// pixel alignment.
    #[inline]
    pub fn float(self, size: u32) -> f64 {
        self.fraction() * size as f64
    }

    /// Get a float for the fraction this represents.
    #[inline]
    pub fn fraction(self) -> f64 {
        match self {
            Fraction::Start | Fraction::Left | Fraction::Top | Fraction::Zero => 0.0,

            Fraction::Eighth | Fraction::OneEighth => 0.125,

            Fraction::Quarter | Fraction::OneQuarter | Fraction::TwoEighths => 0.25,

            Fraction::Third | Fraction::OneThird => 1.0 / 3.0,

            Fraction::ThreeEighths => 0.375,

            Fraction::Half
            | Fraction::OneHalf
            | Fraction::TwoQuarters
            | Fraction::FourEighths
            | Fraction::Center
            | Fraction::Middle => 0.5,

            Fraction::FiveEighths => 0.625,

            Fraction::TwoThirds => 2.0 / 3.0,

            Fraction::ThreeQuarters | Fraction::SixEighths => 0.75,

            Fraction::SevenEighths => 0.875,

            Fraction::End | Fraction::Right | Fraction::Bottom | Fraction::One | Fraction::Full => {
                1.0
            }
        }
    }
}

/// Fill a section of the cell, specified by a
/// horizontal and vertical pair of fraction lines.
pub fn fill(
    metrics: Metrics,
    canvas: &mut Canvas,
    x0: Fraction,
    x1: Fraction,
    y0: Fraction,
    y1: Fraction,
) {
    canvas.r#box(
        x0.min(metrics.cell_width),
        y0.min(metrics.cell_height),
        x1.max(metrics.cell_width),
        y1.max(metrics.cell_height),
        Color::ON,
    );
}

/// Centered vertical line of the provided thickness.
pub fn vline_middle(metrics: Metrics, canvas: &mut Canvas, thickness: Thickness) {
    let thick_px = thickness.height(metrics.box_thickness);
    vline(
        canvas,
        0,
        metrics.cell_height as i32,
        (metrics.cell_width.saturating_sub(thick_px) / 2) as i32,
        thick_px,
    );
}

/// Centered horizontal line of the provided thickness.
pub fn hline_middle(metrics: Metrics, canvas: &mut Canvas, thickness: Thickness) {
    let thick_px = thickness.height(metrics.box_thickness);
    hline(
        canvas,
        0,
        metrics.cell_width as i32,
        (metrics.cell_height.saturating_sub(thick_px) / 2) as i32,
        thick_px,
    );
}

/// Vertical line with the left edge at `x`, between `y1` and `y2`.
pub fn vline(canvas: &mut Canvas, y1: i32, y2: i32, x: i32, thickness_px: u32) {
    canvas.r#box(x, y1, x + thickness_px as i32, y2, Color::ON);
}

/// Horizontal line with the top edge at `y`, between `x1` and `x2`.
pub fn hline(canvas: &mut Canvas, x1: i32, x2: i32, y: i32, thickness_px: u32) {
    canvas.r#box(x1, y, x2, y + thickness_px as i32, Color::ON);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fraction_min_max() {
        // See the explanation in `Fraction::min`.
        assert_eq!(4, Fraction::Half.max(7));
        assert_eq!(3, Fraction::Half.min(7));
        assert_eq!(0, Fraction::Zero.min(7));
        assert_eq!(7, Fraction::End.max(7));
        assert_eq!(3.5, Fraction::Half.float(7));
        assert_eq!(Fraction::EIGHTHS[4].fraction(), 0.5);
    }

    #[test]
    fn thickness_height() {
        assert_eq!(1, Thickness::SuperLight.height(1));
        assert_eq!(2, Thickness::SuperLight.height(4));
        assert_eq!(3, Thickness::Light.height(3));
        assert_eq!(6, Thickness::Heavy.height(3));
    }
}
