//! Port of `src/font/Metrics.zig`.
//!
//! STATUS: ported

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    /// Recommended cell width and height for a monospace grid using this font.
    pub cell_width: u32,
    pub cell_height: u32,

    /// Distance in pixels from the bottom of the cell to the text baseline.
    pub cell_baseline: u32,

    /// Distance in pixels from the top of the cell to the top of the underline.
    pub underline_position: u32,
    /// Thickness in pixels of the underline.
    pub underline_thickness: u32,

    /// Distance in pixels from the top of the cell to the top of the strikethrough.
    pub strikethrough_position: u32,
    /// Thickness in pixels of the strikethrough.
    pub strikethrough_thickness: u32,

    /// Distance in pixels from the top of the cell to the top of the overline.
    /// Can be negative to adjust the position above the top of the cell.
    pub overline_position: i32,
    /// Thickness in pixels of the overline.
    pub overline_thickness: u32,

    /// Thickness in pixels of box drawing characters.
    pub box_thickness: u32,

    /// The thickness in pixels of the cursor sprite. This has a default value
    /// because it is not determined by fonts but rather by user configuration.
    /// (Zig default: `1`.)
    pub cursor_thickness: u32,

    /// The height in pixels of the cursor sprite.
    pub cursor_height: u32,

    /// The constraint height for nerd fonts icons.
    pub icon_height: f64,

    /// The constraint height for nerd fonts icons limited to a single cell width.
    pub icon_height_single: f64,

    /// The unrounded face width, used in scaling calculations.
    pub face_width: f64,

    /// The unrounded face height, used in scaling calculations.
    pub face_height: f64,

    /// The offset from the bottom of the cell to the bottom
    /// of the face's bounding box, based on the rounded and
    /// potentially adjusted cell height.
    pub face_y: f64,
}

/// Minimum acceptable values for some fields to prevent modifiers
/// from being able to, for example, cause 0-thickness underlines.
struct Minimums;

impl Minimums {
    const CELL_WIDTH: u32 = 1;
    const CELL_HEIGHT: u32 = 1;
    const UNDERLINE_THICKNESS: u32 = 1;
    const STRIKETHROUGH_THICKNESS: u32 = 1;
    const OVERLINE_THICKNESS: u32 = 1;
    const BOX_THICKNESS: u32 = 1;
    const CURSOR_THICKNESS: u32 = 1;
    const CURSOR_HEIGHT: u32 = 1;
    const ICON_HEIGHT: f64 = 1.0;
    const ICON_HEIGHT_SINGLE: f64 = 1.0;
    const FACE_HEIGHT: f64 = 1.0;
    const FACE_WIDTH: f64 = 1.0;
}

/// Metrics extracted from a font face, based on
/// the metadata tables and glyph measurements.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct FaceMetrics {
    /// Pixels per em, dividing the other values in this struct by this should
    /// yield sizes in ems, to allow comparing metrics from faces of different
    /// sizes.
    pub px_per_em: f64,

    /// The minimum cell width that can contain any glyph in the ASCII range.
    ///
    /// Determined by measuring all printable glyphs in the ASCII range.
    pub cell_width: f64,

    /// The typographic ascent metric from the font.
    /// This represents the maximum vertical position of the highest ascender.
    ///
    /// Relative to the baseline, in px, +Y=up
    pub ascent: f64,

    /// The typographic descent metric from the font.
    /// This represents the minimum vertical position of the lowest descender.
    ///
    /// Relative to the baseline, in px, +Y=up
    ///
    /// Note:
    /// As this value is generally below the baseline, it is typically negative.
    pub descent: f64,

    /// The typographic line gap (aka "leading") metric from the font.
    /// This represents the additional space to be added between lines in
    /// addition to the space defined by the ascent and descent metrics.
    ///
    /// Positive value in px
    pub line_gap: f64,

    /// The TOP of the underline stroke.
    ///
    /// Relative to the baseline, in px, +Y=up
    pub underline_position: Option<f64>,

    /// The thickness of the underline stroke in px.
    pub underline_thickness: Option<f64>,

    /// The TOP of the strikethrough stroke.
    ///
    /// Relative to the baseline, in px, +Y=up
    pub strikethrough_position: Option<f64>,

    /// The thickness of the strikethrough stroke in px.
    pub strikethrough_thickness: Option<f64>,

    /// The height of capital letters in the font, either derived from
    /// a provided cap height metric or measured from the height of the
    /// capital H glyph.
    pub cap_height: Option<f64>,

    /// The height of lowercase letters in the font, either derived from
    /// a provided ex height metric or measured from the height of the
    /// lowercase x glyph.
    pub ex_height: Option<f64>,

    /// The measured height of the bounding box containing all printable
    /// ASCII characters. This can be different from ascent - descent for
    /// two reasons: non-letter symbols like @ and $ often exceed the
    /// the ascender and descender lines; and fonts often bake the line
    /// gap into the ascent and descent metrics (as per, e.g., the Google
    /// Fonts guidelines: https://simoncozens.github.io/gf-docs/metrics.html).
    ///
    /// Positive value in px
    pub ascii_height: Option<f64>,

    /// The width of the character "水" (CJK water ideograph, U+6C34),
    /// if present. This is used for font size adjustment, to normalize
    /// the width of CJK fonts mixed with latin fonts.
    ///
    /// NOTE: IC = Ideograph Character
    pub ic_width: Option<f64>,
}

impl FaceMetrics {
    /// Convenience function for getting the line height
    /// (ascent - descent + line_gap).
    #[inline]
    pub fn line_height(&self) -> f64 {
        self.ascent - self.descent + self.line_gap
    }

    /// Convenience function for getting the cap height. If this is not
    /// defined in the font, we estimate it as 75% of the ascent.
    #[inline]
    pub fn cap_height(&self) -> f64 {
        if let Some(value) = self.cap_height {
            if value > 0.0 {
                return value;
            }
        }
        0.75 * self.ascent
    }

    /// Convenience function for getting the ex height. If this is not
    /// defined in the font, we estimate it as 75% of the cap height.
    #[inline]
    pub fn ex_height(&self) -> f64 {
        if let Some(value) = self.ex_height {
            if value > 0.0 {
                return value;
            }
        }
        0.75 * self.cap_height()
    }

    /// Convenience function for getting the ASCII height. If we
    /// couldn't measure this, we use 1.5 * cap_height as our
    /// estimator, based on measurements across programming fonts.
    #[inline]
    pub fn ascii_height(&self) -> f64 {
        if let Some(value) = self.ascii_height {
            if value > 0.0 {
                return value;
            }
        }
        1.5 * self.cap_height()
    }

    /// Convenience function for getting the ideograph width. If this is
    /// not defined in the font, we estimate it as the minimum of the
    /// ascii height and two cell widths.
    #[inline]
    pub fn ic_width(&self) -> f64 {
        if let Some(value) = self.ic_width {
            if value > 0.0 {
                return value;
            }
        }
        self.ascii_height().min(2.0 * self.cell_width)
    }

    /// Convenience function for getting the underline thickness. If
    /// this is not defined in the font, we estimate it as 15% of the ex
    /// height.
    #[inline]
    pub fn underline_thickness(&self) -> f64 {
        if let Some(value) = self.underline_thickness {
            if value > 0.0 {
                return value;
            }
        }
        0.15 * self.ex_height()
    }

    /// Convenience function for getting the strikethrough thickness. If
    /// this is not defined in the font, we set it equal to the
    /// underline thickness.
    #[inline]
    pub fn strikethrough_thickness(&self) -> f64 {
        if let Some(value) = self.strikethrough_thickness {
            if value > 0.0 {
                return value;
            }
        }
        self.underline_thickness()
    }

    // NOTE: The getters below return positions, not sizes, so both
    // positive and negative values are valid, hence no sign validation.

    /// Convenience function for getting the underline position. If
    /// this is not defined in the font, we place it one underline
    /// thickness below the baseline.
    #[inline]
    pub fn underline_position(&self) -> f64 {
        self.underline_position
            .unwrap_or_else(|| -self.underline_thickness())
    }

    /// Convenience function for getting the strikethrough position. If
    /// this is not defined in the font, we center it at half the ex
    /// height, so that it's perfectly centered on lower case text.
    #[inline]
    pub fn strikethrough_position(&self) -> f64 {
        self.strikethrough_position
            .unwrap_or_else(|| (self.ex_height() + self.strikethrough_thickness()) * 0.5)
    }
}

/// Zig `@round`: rounds half away from zero (same as `f64::round`).
#[inline]
fn round(v: f64) -> f64 {
    v.round()
}

impl Metrics {
    /// Calculate our metrics based on values extracted from a font.
    ///
    /// Try to pass values with as much precision as possible,
    /// do not round them before using them for this function.
    ///
    /// For any nullable options that are not provided, estimates will be used.
    pub fn calc(face: FaceMetrics) -> Metrics {
        // These are the unrounded advance width and line height values,
        // which are retained separately from the rounded cell width and
        // height values (below), for calculations that need to know how
        // much error there is between the design dimensions of the font
        // and the pixel dimensions of our cells.
        let face_width = face.cell_width;
        let face_height = face.line_height();

        // The cell width and height values need to be integers since they
        // represent pixel dimensions of the grid cells in the terminal.
        //
        // We use @round for the cell width to limit the difference from
        // the "true" width value to no more than 0.5px. This is a better
        // approximation of the authorial intent of the font than ceiling
        // would be, and makes the apparent spacing match better between
        // low and high DPI displays.
        //
        // This does mean that it's possible for a glyph to overflow the
        // edge of the cell by a pixel if it has no side bearings, but in
        // reality such glyphs are generally meant to connect to adjacent
        // glyphs in some way so it's not really an issue.
        //
        // The same is true for the height. Some fonts are poorly authored
        // and have a descender on a normal glyph that extends right up to
        // the descent value of the face, and this can result in the glyph
        // overflowing the bottom of the cell by a pixel, which isn't good
        // but if we try to prevent it by increasing the cell height then
        // we get line heights that are too large for most users and even
        // more inconsistent across DPIs.
        //
        // Users who experience such cell-height overflows should:
        //
        // 1. Nag the font author to either redesign the glyph to not go
        //    so low, or else adjust the descent value in the metadata.
        //
        // 2. Add an `adjust-cell-height` entry to their config to give
        //    the cell enough room for the glyph.
        let cell_width = round(face_width);
        let cell_height = round(face_height);

        // We split our line gap in two parts, and put half of it on the top
        // of the cell and the other half on the bottom, so that our text never
        // bumps up against either edge of the cell vertically.
        let half_line_gap = face.line_gap / 2.0;

        // NOTE: Unlike all our other metrics, `cell_baseline` is
        // relative to the BOTTOM of the cell rather than the top.
        let face_baseline = half_line_gap - face.descent;
        // We calculate the baseline by trying to center the face vertically
        // in the pixel-rounded cell height, so that before rounding it will
        // be an even distance from the top and bottom of the cell, meaning
        // it either sticks out the same amount or is inset the same amount,
        // depending on whether the cell height was rounded up or down from
        // the line height. We do this by adding half the difference between
        // the cell height and the face height.
        let cell_baseline = round(face_baseline - (cell_height - face_height) / 2.0);

        // We keep track of the offset from the bottom of the cell
        // to the bottom of the face's "true" bounding box, which at
        // this point, since nothing has been scaled yet, is equivalent
        // to the offset between the baseline we draw at (cell_baseline)
        // and the one the font wants (face_baseline).
        let face_y = cell_baseline - face_baseline;

        // We calculate a top_to_baseline to make following calculations simpler.
        let top_to_baseline = cell_height - cell_baseline;

        // Get the other font metrics or their estimates. See doc comments
        // in FaceMetrics for explanations of the estimation heuristics.
        let cap_height = face.cap_height();
        let underline_thickness = face.underline_thickness().ceil().max(1.0);
        let strikethrough_thickness = face.strikethrough_thickness().ceil().max(1.0);
        let underline_position = round(top_to_baseline - face.underline_position());
        let strikethrough_position = round(top_to_baseline - face.strikethrough_position());

        // Same heuristic as the font_patcher script. We store icon_height
        // separately from face_height such that modifiers can apply to the former
        // without affecting the latter.
        let icon_height = face_height;
        let icon_height_single = (2.0 * cap_height + face_height) / 3.0;

        let mut result = Metrics {
            cell_width: cell_width as u32,
            cell_height: cell_height as u32,
            cell_baseline: cell_baseline as u32,
            underline_position: underline_position as u32,
            underline_thickness: underline_thickness as u32,
            strikethrough_position: strikethrough_position as u32,
            strikethrough_thickness: strikethrough_thickness as u32,
            overline_position: 0,
            overline_thickness: underline_thickness as u32,
            box_thickness: underline_thickness as u32,
            cursor_thickness: 1,
            cursor_height: cell_height as u32,
            icon_height,
            icon_height_single,
            face_width,
            face_height,
            face_y,
        };

        // Ensure all metrics are within their allowable range.
        result.clamp();

        // std.log.debug("metrics={}", .{result});

        result
    }

    /// Apply a set of modifiers.
    pub fn apply(&mut self, mods: &ModifierSet) {
        for (key, modifier) in mods.iter() {
            let modifier = *modifier;
            match *key {
                // We clamp these values to a minimum of 1 to prevent divide-by-zero
                // in downstream operations.
                Key::CellWidth | Key::CellHeight => {
                    let field = if *key == Key::CellWidth {
                        &mut self.cell_width
                    } else {
                        &mut self.cell_height
                    };

                    // Compute the new value. If it is the same avoid the work.
                    let original = *field;
                    let new = modifier.apply(original).max(1);
                    if new == original {
                        continue;
                    }

                    // Set the new value
                    *field = new;

                    // For cell height, we have to also modify some positions
                    // that are absolute from the top of the cell. The main goal
                    // here is to center the baseline so that text is vertically
                    // centered in the cell.
                    if *key == Key::CellHeight {
                        let original_f64 = original as f64;
                        let new_f64 = new as f64;
                        let diff = new_f64 - original_f64;
                        let half_diff = diff / 2.0;

                        // If the diff is even, the number of pixels we add
                        // will be the same for the top and the bottom, but
                        // if the diff is odd then we want to add the extra
                        // pixel to the edge of the cell that needs it most.
                        //
                        // How much the edge "needs it" depends on whether
                        // the face is higher or lower than it should be to
                        // be perfectly centered in the cell.
                        //
                        // If the face were perfectly centered then face_y
                        // would be equal to half of the difference between
                        // the cell height and the face height.
                        let position_with_respect_to_center =
                            self.face_y - (original_f64 - self.face_height) / 2.0;

                        let (diff_top, diff_bottom) = if position_with_respect_to_center > 0.0 {
                            // The baseline is higher than it should be, so we
                            // add the extra to the top, or if it's a negative
                            // diff it gets added to the bottom because of how
                            // floor and ceil work.
                            (half_diff.ceil(), half_diff.floor())
                        } else {
                            // The baseline is lower than it should be, so we
                            // add the extra to the bottom, or vice versa for
                            // negative diffs.
                            (half_diff.floor(), half_diff.ceil())
                        };

                        // The cell baseline and face_y values are relative to the
                        // bottom of the cell so we add the bottom diff to them.
                        add_float_to_int(&mut self.cell_baseline, diff_bottom);
                        self.face_y += diff_bottom;

                        // These are all relative to the top of the cell.
                        add_float_to_int(&mut self.underline_position, diff_top);
                        add_float_to_int(&mut self.strikethrough_position, diff_top);
                        self.overline_position =
                            self.overline_position.saturating_add(diff_top as i32);
                    }
                }
                Key::IconHeight => {
                    self.icon_height = modifier.apply(self.icon_height);
                    self.icon_height_single = modifier.apply(self.icon_height_single);
                }

                Key::CellBaseline => self.cell_baseline = modifier.apply(self.cell_baseline),
                Key::UnderlinePosition => {
                    self.underline_position = modifier.apply(self.underline_position)
                }
                Key::UnderlineThickness => {
                    self.underline_thickness = modifier.apply(self.underline_thickness)
                }
                Key::StrikethroughPosition => {
                    self.strikethrough_position = modifier.apply(self.strikethrough_position)
                }
                Key::StrikethroughThickness => {
                    self.strikethrough_thickness = modifier.apply(self.strikethrough_thickness)
                }
                Key::OverlinePosition => {
                    self.overline_position = modifier.apply(self.overline_position)
                }
                Key::OverlineThickness => {
                    self.overline_thickness = modifier.apply(self.overline_thickness)
                }
                Key::BoxThickness => self.box_thickness = modifier.apply(self.box_thickness),
                Key::CursorThickness => {
                    self.cursor_thickness = modifier.apply(self.cursor_thickness)
                }
                Key::CursorHeight => self.cursor_height = modifier.apply(self.cursor_height),
                Key::IconHeightSingle => {
                    self.icon_height_single = modifier.apply(self.icon_height_single)
                }
                Key::FaceWidth => self.face_width = modifier.apply(self.face_width),
                Key::FaceHeight => self.face_height = modifier.apply(self.face_height),
                Key::FaceY => self.face_y = modifier.apply(self.face_y),
            }
        }

        // Prevent modifiers from pushing metrics out of their allowable range.
        self.clamp();
    }

    /// Clamp all metrics to their allowable range.
    fn clamp(&mut self) {
        self.cell_width = self.cell_width.max(Minimums::CELL_WIDTH);
        self.cell_height = self.cell_height.max(Minimums::CELL_HEIGHT);
        self.underline_thickness = self.underline_thickness.max(Minimums::UNDERLINE_THICKNESS);
        self.strikethrough_thickness = self
            .strikethrough_thickness
            .max(Minimums::STRIKETHROUGH_THICKNESS);
        self.overline_thickness = self.overline_thickness.max(Minimums::OVERLINE_THICKNESS);
        self.box_thickness = self.box_thickness.max(Minimums::BOX_THICKNESS);
        self.cursor_thickness = self.cursor_thickness.max(Minimums::CURSOR_THICKNESS);
        self.cursor_height = self.cursor_height.max(Minimums::CURSOR_HEIGHT);
        self.icon_height = self.icon_height.max(Minimums::ICON_HEIGHT);
        self.icon_height_single = self.icon_height_single.max(Minimums::ICON_HEIGHT_SINGLE);
        self.face_height = self.face_height.max(Minimums::FACE_HEIGHT);
        self.face_width = self.face_width.max(Minimums::FACE_WIDTH);
    }

    // NOTE: This is purposely not pub because we want to force outside callers
    // to use the `.{}` syntax so unused fields are detected by the compiler.
    fn init() -> Metrics {
        Metrics {
            cell_width: 0,
            cell_height: 0,
            cell_baseline: 0,
            underline_position: 0,
            underline_thickness: 0,
            strikethrough_position: 0,
            strikethrough_thickness: 0,
            overline_position: 0,
            overline_thickness: 0,
            box_thickness: 0,
            cursor_thickness: 1,
            cursor_height: 0,
            icon_height: 0.0,
            icon_height_single: 0.0,
            face_width: 0.0,
            face_height: 0.0,
            face_y: 0.0,
        }
    }
}

/// Helper function for adding an f64 to a u32.
///
/// Performs saturating addition or subtraction
/// depending on the sign of the provided float.
///
/// The f64 is asserted to have an integer value.
#[inline]
fn add_float_to_int(int: &mut u32, float: f64) {
    debug_assert!(float.floor() == float);
    *int = if float >= 0.0 {
        int.saturating_add(float as u32)
    } else {
        int.saturating_sub((-float) as u32)
    };
}

/// A set of modifiers to apply to metrics. We use a hash map because
/// we expect most metrics to be unmodified and want to take up as
/// little space as possible.
pub type ModifierSet = HashMap<Key, Modifier>;

/// A modifier to apply to a metrics value. The modifier value represents
/// a delta, so percent is a percentage to change, not a percentage of.
/// For example, "20%" is 20% larger, not 20% of the value. Likewise,
/// an absolute value of "20" is 20 larger, not literally 20.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Modifier {
    Percent(f64),
    Absolute(i32),
}

/// Errors from [`Modifier::parse`] / [`Modifier::parse_cli`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModifierParseError {
    InvalidFormat,
    ValueRequired,
}

impl std::fmt::Display for ModifierParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModifierParseError::InvalidFormat => f.write_str("InvalidFormat"),
            ModifierParseError::ValueRequired => f.write_str("ValueRequired"),
        }
    }
}

impl std::error::Error for ModifierParseError {}

/// Zig's `std.fmt.parseInt`/`parseFloat` accept `_` between digits; strip
/// them (only when they are between two digits) so we accept the same
/// inputs. Returns `None` if an underscore is misplaced.
fn strip_digit_underscores(input: &str) -> Option<std::borrow::Cow<'_, str>> {
    if !input.contains('_') {
        return Some(std::borrow::Cow::Borrowed(input));
    }
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'_' {
            let prev_ok = i > 0 && bytes[i - 1].is_ascii_hexdigit();
            let next_ok = i + 1 < bytes.len() && bytes[i + 1].is_ascii_hexdigit();
            if !prev_ok || !next_ok {
                return None;
            }
            continue;
        }
        out.push(b as char);
    }
    Some(std::borrow::Cow::Owned(out))
}

/// A numeric type that a [`Modifier`] can be applied to (Zig `anytype`
/// in `Modifier.apply`).
pub trait ModifierValue: Copy {
    fn apply_modifier(self, m: Modifier) -> Self;
}

macro_rules! impl_modifier_value_int {
    ($t:ty, $signed:expr) => {
        impl ModifierValue for $t {
            fn apply_modifier(self, m: Modifier) -> Self {
                match m {
                    Modifier::Percent(p) => {
                        let p_clamped: f64 = p.max(0.0);
                        let v_f64 = self as f64;
                        let applied_f64 = (v_f64 * p_clamped).round();
                        applied_f64 as $t
                    }
                    Modifier::Absolute(abs) => {
                        let v_i64 = self as i64;
                        let abs_i64 = abs as i64;
                        let applied_i64 = v_i64.saturating_add(abs_i64);
                        let clamped_i64: i64 = if $signed {
                            applied_i64
                        } else {
                            applied_i64.max(0)
                        };
                        <$t>::try_from(clamped_i64)
                            .unwrap_or_else(|_| <$t>::MAX * (clamped_i64.signum() as $t))
                    }
                }
            }
        }
    };
}

impl_modifier_value_int!(u32, false);
impl_modifier_value_int!(i32, true);

impl ModifierValue for f64 {
    fn apply_modifier(self, m: Modifier) -> Self {
        match m {
            Modifier::Percent(p) => self * p.max(0.0),
            Modifier::Absolute(abs) => self + abs as f64,
        }
    }
}

impl Modifier {
    /// Parses the modifier value. If the value ends in "%" it is assumed
    /// to be a percent, otherwise the value is parsed as an integer.
    pub fn parse(input: &str) -> Result<Modifier, ModifierParseError> {
        if input.is_empty() {
            return Err(ModifierParseError::InvalidFormat);
        }

        if let Some(num) = input.strip_suffix('%') {
            let num = strip_digit_underscores(num).ok_or(ModifierParseError::InvalidFormat)?;
            let mut percent: f64 = num
                .parse::<f64>()
                .map_err(|_| ModifierParseError::InvalidFormat)?;
            percent /= 100.0;

            if percent <= -1.0 {
                return Ok(Modifier::Percent(0.0));
            }
            if percent < 0.0 {
                return Ok(Modifier::Percent(1.0 + percent));
            }
            return Ok(Modifier::Percent(1.0 + percent));
        }

        let num = strip_digit_underscores(input).ok_or(ModifierParseError::InvalidFormat)?;
        Ok(Modifier::Absolute(
            num.parse::<i32>()
                .map_err(|_| ModifierParseError::InvalidFormat)?,
        ))
    }

    /// So it works with the config framework.
    pub fn parse_cli(input: Option<&str>) -> Result<Modifier, ModifierParseError> {
        Self::parse(input.ok_or(ModifierParseError::ValueRequired)?)
    }

    /// Used by config formatter
    pub fn format_entry(
        &self,
        formatter: &mut crate::config::formatter::EntryFormatter<'_>,
    ) -> Result<(), crate::config::formatter::Error> {
        match *self {
            Modifier::Percent(v) => formatter.format_entry(format!("{}%", (v - 1.0) * 100.0).as_str()),
            Modifier::Absolute(v) => formatter.format_entry(format!("{}", v).as_str()),
        }
    }

    /// Apply a modifier to a numeric value.
    pub fn apply<T: ModifierValue>(self, v: T) -> T {
        v.apply_modifier(self)
    }

    /// Hash using the hasher.
    pub fn hash<H: std::hash::Hasher>(&self, hasher: &mut H) {
        match *self {
            // floats can't be hashed directly so we bitcast to i64.
            // for the purpose of what we're trying to do this seems
            // good enough but I would prefer value hashing.
            Modifier::Percent(v) => {
                hasher.write_u8(0);
                hasher.write_i64(v.to_bits() as i64);
            }
            Modifier::Absolute(v) => {
                hasher.write_u8(1);
                hasher.write_i32(v);
            }
        }
    }
}

/// Key is an enum of all the available metrics keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Key {
    CellWidth,
    CellHeight,
    CellBaseline,
    UnderlinePosition,
    UnderlineThickness,
    StrikethroughPosition,
    StrikethroughThickness,
    OverlinePosition,
    OverlineThickness,
    BoxThickness,
    CursorThickness,
    CursorHeight,
    IconHeight,
    IconHeightSingle,
    FaceWidth,
    FaceHeight,
    FaceY,
}

impl Key {
    /// All keys in declaration order.
    pub const ALL: [Key; 17] = [
        Key::CellWidth,
        Key::CellHeight,
        Key::CellBaseline,
        Key::UnderlinePosition,
        Key::UnderlineThickness,
        Key::StrikethroughPosition,
        Key::StrikethroughThickness,
        Key::OverlinePosition,
        Key::OverlineThickness,
        Key::BoxThickness,
        Key::CursorThickness,
        Key::CursorHeight,
        Key::IconHeight,
        Key::IconHeightSingle,
        Key::FaceWidth,
        Key::FaceHeight,
        Key::FaceY,
    ];

    /// The Zig field name of this key.
    pub const fn as_str(self) -> &'static str {
        match self {
            Key::CellWidth => "cell_width",
            Key::CellHeight => "cell_height",
            Key::CellBaseline => "cell_baseline",
            Key::UnderlinePosition => "underline_position",
            Key::UnderlineThickness => "underline_thickness",
            Key::StrikethroughPosition => "strikethrough_position",
            Key::StrikethroughThickness => "strikethrough_thickness",
            Key::OverlinePosition => "overline_position",
            Key::OverlineThickness => "overline_thickness",
            Key::BoxThickness => "box_thickness",
            Key::CursorThickness => "cursor_thickness",
            Key::CursorHeight => "cursor_height",
            Key::IconHeight => "icon_height",
            Key::IconHeightSingle => "icon_height_single",
            Key::FaceWidth => "face_width",
            Key::FaceHeight => "face_height",
            Key::FaceY => "face_y",
        }
    }
}

impl std::str::FromStr for Key {
    type Err = ();

    fn from_str(s: &str) -> Result<Key, ()> {
        Key::ALL.iter().copied().find(|k| k.as_str() == s).ok_or(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::config::formatter::entry_formatter;

    #[test]
    fn format_config_percent() {
        let mut buf = String::new();
        let p = Modifier::parse_cli(Some("24%")).unwrap();
        p.format_entry(&mut entry_formatter("a", &mut buf)).unwrap();
        assert_eq!("a = 24%\n", buf);
    }

    #[test]
    fn format_config_absolute() {
        let mut buf = String::new();
        let p = Modifier::parse_cli(Some("-30")).unwrap();
        p.format_entry(&mut entry_formatter("a", &mut buf)).unwrap();
        assert_eq!("a = -30\n", buf);
    }

    #[test]
    fn metrics_apply_modifiers() {
        let mut set: ModifierSet = ModifierSet::new();
        set.insert(Key::CellWidth, Modifier::Percent(1.2));

        let mut m = Metrics::init();
        m.cell_width = 100;
        m.apply(&set);
        assert_eq!(120u32, m.cell_width);
    }

    #[test]
    fn metrics_adjust_cell_height_smaller() {
        let mut set: ModifierSet = ModifierSet::new();
        // We choose numbers such that the subtracted number of pixels is odd,
        // as that's the case that could most easily have off-by-one errors.
        // Here we're removing 25 pixels: 13 on the bottom, 12 on top, split
        // that way because we're simulating a face that's 0.33px higher than
        // it "should" be (due to rounding).
        set.insert(Key::CellHeight, Modifier::Percent(0.75));

        let mut m = Metrics::init();
        m.face_y = 0.33;
        m.cell_baseline = 50;
        m.underline_position = 55;
        m.strikethrough_position = 30;
        m.overline_position = 0;
        m.cell_height = 100;
        m.face_height = 99.67;
        m.cursor_height = 100;
        m.apply(&set);
        assert_eq!(-12.67, m.face_y);
        assert_eq!(75u32, m.cell_height);
        assert_eq!(37u32, m.cell_baseline);
        assert_eq!(43u32, m.underline_position);
        assert_eq!(18u32, m.strikethrough_position);
        assert_eq!(-12i32, m.overline_position);
        // Cursor height is separate from cell height and does not follow it.
        assert_eq!(100u32, m.cursor_height);
    }

    #[test]
    fn metrics_adjust_cell_height_larger() {
        let mut set: ModifierSet = ModifierSet::new();
        // We choose numbers such that the added number of pixels is odd,
        // as that's the case that could most easily have off-by-one errors.
        // Here we're adding 75 pixels: 37 on the bottom, 38 on top, split
        // that way because we're simulating a face that's 0.33px higher
        // than it "should" be (due to rounding).
        set.insert(Key::CellHeight, Modifier::Percent(1.75));

        let mut m = Metrics::init();
        m.face_y = 0.33;
        m.cell_baseline = 50;
        m.underline_position = 55;
        m.strikethrough_position = 30;
        m.overline_position = 0;
        m.cell_height = 100;
        m.face_height = 99.67;
        m.cursor_height = 100;
        m.apply(&set);
        assert_eq!(37.33, m.face_y);
        assert_eq!(175u32, m.cell_height);
        assert_eq!(87u32, m.cell_baseline);
        assert_eq!(93u32, m.underline_position);
        assert_eq!(68u32, m.strikethrough_position);
        assert_eq!(38i32, m.overline_position);
        // Cursor height is separate from cell height and does not follow it.
        assert_eq!(100u32, m.cursor_height);
    }

    #[test]
    fn metrics_adjust_icon_height_by_percentage() {
        let mut set: ModifierSet = ModifierSet::new();
        set.insert(Key::IconHeight, Modifier::Percent(0.75));

        let mut m = Metrics::init();
        m.icon_height = 100.0;
        m.icon_height_single = 80.0;
        m.face_height = 100.0;
        m.face_y = 1.0;
        m.apply(&set);
        assert_eq!(75.0, m.icon_height);
        assert_eq!(60.0, m.icon_height_single);
        // Face metrics not affected
        assert_eq!(100.0, m.face_height);
        assert_eq!(1.0, m.face_y);
    }

    #[test]
    fn metrics_adjust_icon_height_by_absolute_pixels() {
        let mut set: ModifierSet = ModifierSet::new();
        set.insert(Key::IconHeight, Modifier::Absolute(-5));

        let mut m = Metrics::init();
        m.icon_height = 100.0;
        m.icon_height_single = 80.0;
        m.face_height = 100.0;
        m.face_y = 1.0;
        m.apply(&set);
        assert_eq!(95.0, m.icon_height);
        assert_eq!(75.0, m.icon_height_single);
        // Face metrics not affected
        assert_eq!(100.0, m.face_height);
        assert_eq!(1.0, m.face_y);
    }

    #[test]
    fn modifier_parse_absolute() {
        {
            let m = Modifier::parse("100").unwrap();
            assert_eq!(Modifier::Absolute(100), m);
        }

        {
            let m = Modifier::parse("-100").unwrap();
            assert_eq!(Modifier::Absolute(-100), m);
        }
    }

    #[test]
    fn modifier_parse_percent() {
        {
            let m = Modifier::parse("20%").unwrap();
            assert_eq!(Modifier::Percent(1.2), m);
        }
        {
            let m = Modifier::parse("-20%").unwrap();
            assert_eq!(Modifier::Percent(0.8), m);
        }
        {
            let m = Modifier::parse("0%").unwrap();
            assert_eq!(Modifier::Percent(1.0), m);
        }
    }

    #[test]
    fn modifier_percent() {
        {
            let m = Modifier::Percent(0.8);
            let v: u32 = m.apply(100u32);
            assert_eq!(80u32, v);
        }
        {
            let m = Modifier::Percent(1.8);
            let v: u32 = m.apply(100u32);
            assert_eq!(180u32, v);
        }
    }

    #[test]
    fn modifier_absolute() {
        {
            let m = Modifier::Absolute(-100);
            let v: u32 = m.apply(100u32);
            assert_eq!(0u32, v);
        }
        {
            let m = Modifier::Absolute(-120);
            let v: u32 = m.apply(100u32);
            assert_eq!(0u32, v);
        }
        {
            let m = Modifier::Absolute(100);
            let v: u32 = m.apply(100u32);
            assert_eq!(200u32, v);
        }
    }

    #[test]
    fn key_names() {
        for k in Key::ALL {
            assert_eq!(Ok(k), k.as_str().parse::<Key>());
        }
        assert!("nope".parse::<Key>().is_err());
    }
}
