//! Port of `src/font/face/coretext.zig`.
//!
//! STATUS: ported (macOS-only; type-checked with
//! `cargo check --target x86_64-apple-darwin`, tests not run on Linux)

#![cfg(any(target_os = "macos", target_os = "ios"))]

use crate::font::atlas::{self, Atlas};
use crate::font::face::{DesiredSize, NameError, Options, Variation, VariationId};
use crate::font::glyph::{self, ConstraintSize, Glyph, RenderOptions};
use crate::font::library::Library;
use crate::font::metrics::FaceMetrics;
use crate::font::opentype;
use crate::pkg::harfbuzz;
use crate::pkg::macos;
use crate::quirks;
use crate::quirks::inline_assert as assert;

const LOG: &str = "font_face";

/// Errors from initializing a face or rendering a glyph (the Zig inferred
/// error sets).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    OutOfMemory,
    AtlasFull,
    FontInitFailure,
    InvalidAtlasFormat,
    InvalidSVGTable,
    /// A CoreFoundation/CoreText/CoreGraphics call failed.
    Macos(macos::Error),
    /// A HarfBuzz call failed.
    Harfbuzz(harfbuzz::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for Error {}

impl From<macos::Error> for Error {
    fn from(e: macos::Error) -> Error {
        Error::Macos(e)
    }
}

impl From<harfbuzz::Error> for Error {
    fn from(e: harfbuzz::Error) -> Error {
        Error::Harfbuzz(e)
    }
}

impl From<atlas::Error> for Error {
    fn from(e: atlas::Error) -> Error {
        match e {
            atlas::Error::OutOfMemory => Error::OutOfMemory,
            atlas::Error::AtlasFull => Error::AtlasFull,
        }
    }
}

/// True if our build is using Harfbuzz. If we're not, we can avoid
/// some Harfbuzz-specific code paths.
const HARFBUZZ_SHAPER: bool = crate::font::OPTIONS.backend.has_harfbuzz();

/// The matrix applied to a regular font to auto-italicize it.
pub const ITALIC_SKEW: macos::graphics::AffineTransform = macos::graphics::AffineTransform {
    a: 1.0,
    b: 0.0,
    c: 0.267949, // approx. tan(15)
    d: 1.0,
    tx: 0.0,
    ty: 0.0,
};

#[derive(Debug)]
pub struct Face {
    /// Our font face
    pub font: &'static macos::text::Font,

    /// Harfbuzz font corresponding to this face. We only use this
    /// if we're using Harfbuzz.
    pub hb_font: Option<harfbuzz::Font>,

    /// Set quirks.disableDefaultFontFeatures
    pub quirks_disable_default_font_features: bool,

    /// True if this font face should be rasterized with a synthetic bold
    /// effect. This is used for fonts that don't have a bold variant.
    pub synthetic_bold: Option<f64>,

    /// If the face can possibly be colored, then this is the state
    /// used to check for color information. This is null if the font
    /// can't possibly be colored (i.e. doesn't have SVG, sbix, etc
    /// tables).
    pub color: Option<ColorState>,

    /// The current size this font is set to.
    pub size: DesiredSize,
}

impl Face {
    /// Initialize a CoreText-based font from a TTF/TTC in memory.
    pub fn init(lib: Library, source: &'static [u8], opts: Options) -> Result<Face, Error> {
        let _ = lib;

        let data = macos::foundation::Data::create_with_bytes_no_copy(source)?;
        let desc = macos::text::create_font_descriptor_from_data(data);
        data.release();
        let desc = desc.ok_or(Error::FontInitFailure)?;

        let ct_font = macos::text::Font::create_with_font_descriptor(desc, 12.0);
        desc.release();
        let ct_font = ct_font?;

        let result = Self::init_font_copy(ct_font, opts);
        ct_font.release();
        result
    }

    /// Initialize a CoreText-based face from another initialized font face
    /// but with a new size. This is often how CoreText fonts are initialized
    /// because the font is loaded at a default size during discovery, and then
    /// adjusted to the final size for final load.
    pub fn init_font_copy(base: &macos::text::Font, opts: Options) -> Result<Face, Error> {
        // Create a copy. The copyWithAttributes docs say the size is in points,
        // but we need to scale the points by the DPI and to do that we use our
        // function called "pixels".
        let ct_font = base.copy_with_attributes(opts.size.pixels(), None, None)?;
        Self::init_font(ct_font, opts)
    }

    /// Initialize a face with a CTFont. This will take ownership over
    /// the CTFont. This does NOT copy or retain the CTFont.
    pub fn init_font(ct_font: &'static macos::text::Font, opts: Options) -> Result<Face, Error> {
        let traits = ct_font.get_symbolic_traits();

        let hb_font = if HARFBUZZ_SHAPER {
            let mut hb_font = match harfbuzz::coretext::create_font(ct_font) {
                Ok(v) => v,
                Err(err) => {
                    ct_font.release();
                    return Err(err.into());
                }
            };
            let pixels = opentype::sfnt::F26Dot6::from(opts.size.pixels() as f64);
            hb_font.set_scale(pixels.to_bits() as u32, pixels.to_bits() as u32);
            Some(hb_font)
        } else {
            None
        };

        let color = if traits.color_glyphs {
            match ColorState::init(ct_font) {
                Ok(v) => Some(v),
                Err(err) => {
                    if let Some(mut hb) = hb_font {
                        // SAFETY: not used afterwards.
                        unsafe { hb.destroy() };
                    }
                    ct_font.release();
                    return Err(err);
                }
            }
        } else {
            None
        };

        let mut result = Face {
            font: ct_font,
            hb_font,
            quirks_disable_default_font_features: false,
            synthetic_bold: None,
            color,
            size: opts.size,
        };
        result.quirks_disable_default_font_features =
            quirks::disable_default_font_features(&result);

        // In debug mode, we output information about available variation axes,
        // if they exist.
        if cfg!(debug_assertions) {
            if let Some(axes) = ct_font.copy_attribute(macos::text::attr::VariationAxes) {
                let mut buf = [0u8; 1024];
                let name = String::from_utf8_lossy(result.name(&mut buf).unwrap_or(b"")).into_owned();
                log::debug!(target: LOG, "variation axes font={}", name);

                let len = axes.get_count();
                for i in 0..len {
                    use macos::foundation::{Dictionary, Number, NumberType};
                    use macos::text::FontVariationAxisKey as Key;
                    // SAFETY: the variation axes attribute is an array of
                    // dictionaries keyed by `FontVariationAxisKey`, with
                    // string names and numeric values.
                    let (cf_name, cf_id, cf_min, cf_max, cf_def) = unsafe {
                        let dict: &Dictionary = axes.get_value_at_index(i);
                        (
                            dict.get_value::<macos::foundation::String>(Key::Name.key().as_ptr())
                                .unwrap(),
                            dict.get_value::<Number>(Key::Identifier.key().as_ptr()).unwrap(),
                            dict.get_value::<Number>(Key::MinimumValue.key().as_ptr()).unwrap(),
                            dict.get_value::<Number>(Key::MaximumValue.key().as_ptr()).unwrap(),
                            dict.get_value::<Number>(Key::DefaultValue.key().as_ptr()).unwrap(),
                        )
                    };

                    let namestr = cf_name
                        .cstring(&mut buf, macos::foundation::StringEncoding::Utf8)
                        .map(|s| String::from_utf8_lossy(s).into_owned())
                        .unwrap_or_default();

                    let mut id_raw: std::ffi::c_int = 0;
                    let _ = cf_id.get_value(NumberType::Int, &mut id_raw);
                    let id = VariationId::from_bits(id_raw as u32);

                    let mut min: f64 = 0.0;
                    let _ = cf_min.get_value(NumberType::Double, &mut min);

                    let mut max: f64 = 0.0;
                    let _ = cf_max.get_value(NumberType::Double, &mut max);

                    let mut def: f64 = 0.0;
                    let _ = cf_def.get_value(NumberType::Double, &mut def);

                    log::debug!(
                        target: LOG,
                        "variation axis: name={} id={} min={} max={} def={}",
                        namestr,
                        String::from_utf8_lossy(&id.str()),
                        min,
                        max,
                        def,
                    );
                }
                axes.release();
            }
        }

        Ok(result)
    }

    /// Return a new face that is the same as this but has a transformation
    /// matrix applied to italicize it.
    pub fn synthetic_italic(&self, opts: Options) -> Result<Face, Error> {
        let ct_font = self.font.copy_with_attributes(0.0, Some(&ITALIC_SKEW), None)?;
        Self::init_font(ct_font, opts)
    }

    /// Return a new face that is the same as this but applies a synthetic
    /// bold effect to it. This is useful for fonts that don't have a bold
    /// variant.
    pub fn synthetic_bold(&self, opts: Options) -> Result<Face, Error> {
        let ct_font = self.font.copy_with_attributes(0.0, None, None)?;
        let mut face = Self::init_font(ct_font, opts)?;

        // To determine our synthetic bold line width we get a multiplier
        // from the font size in points. This is a heuristic that is based
        // on the fact that a line width of 1 looks good to me at a certain
        // point size. We want to scale that up roughly linearly with the
        // font size.
        let points_f64 = opts.size.points as f64;
        let line_width = (points_f64 / 14.0).max(1.0);
        // log.debug("synthetic bold line width={}", .{line_width});
        face.synthetic_bold = Some(line_width);

        Ok(face)
    }

    /// Returns the font name. If allocation is required, buf will be used,
    /// but sometimes allocation isn't required and a static string is
    /// returned.
    pub fn name<'a>(&'a self, buf: &'a mut [u8]) -> Result<&'a [u8], NameError> {
        let family_name = self.font.copy_family_name();
        if let Some(s) = family_name.cstring_ptr(macos::foundation::StringEncoding::Utf8) {
            return Ok(s.to_bytes());
        }

        // "NULL if the internal storage of theString does not allow
        // this to be returned efficiently." In this case, we need
        // to allocate.
        family_name
            .cstring(buf, macos::foundation::StringEncoding::Utf8)
            .ok_or(NameError::OutOfMemory)
    }

    /// Resize the font in-place. If this succeeds, the caller is responsible
    /// for clearing any glyph caches, font atlas data, etc.
    pub fn set_size(&mut self, opts: Options) -> Result<(), Error> {
        // We just create a copy and replace ourself
        let face = Self::init_font_copy(self.font, opts)?;
        *self = face;
        Ok(())
    }

    /// Set the variation axes for this font. This will modify this font
    /// in-place.
    pub fn set_variations(&mut self, vs: &[Variation], opts: Options) -> Result<(), Error> {
        // If we have no variations, we don't need to do anything.
        if vs.is_empty() {
            return Ok(());
        }

        // Create a new font descriptor with all the variations set.
        let mut desc = self.font.copy_descriptor();
        for v in vs {
            let id_bits = v.id.to_bits();
            let id = macos::foundation::Number::create(macos::foundation::NumberType::Int, &id_bits)?;
            let next = macos::text::FontDescriptor::create_copy_with_variation(desc, id, v.value);
            id.release();
            desc.release();
            desc = next?;
        }

        // Put our current size in the opts so that we don't change size.
        let mut new_opts = opts;
        new_opts.size = self.size;

        // Initialize a font based on these attributes.
        let ct_font = self.font.copy_with_attributes(0.0, None, Some(desc));
        desc.release();
        let face = Self::init_font(ct_font?, new_opts)?;
        *self = face;
        Ok(())
    }

    /// Returns true if the face has any glyphs that are colorized.
    /// To determine if an individual glyph is colorized you must use
    /// isColorGlyph.
    pub fn has_color(&self) -> bool {
        self.color.is_some()
    }

    /// Returns true if the given glyph ID is colorized.
    pub fn is_color_glyph(&self, glyph_id: u32) -> bool {
        let Some(c) = &self.color else {
            return false;
        };
        c.is_color_glyph(glyph_id)
    }

    /// Returns the glyph index for the given Unicode code point. If this
    /// face doesn't support this glyph, null is returned.
    pub fn glyph_index(&self, cp: u32) -> Option<u32> {
        // Turn UTF-32 into UTF-16 for CT API
        let mut unichars = [0u16; 2];
        let pair = macos::foundation::string_get_surrogate_pair_for_long_character(cp, &mut unichars);
        let len: usize = if pair { 2 } else { 1 };

        // Get our glyphs
        let mut glyphs: [macos::graphics::Glyph; 2] = [0, 0];
        if !self
            .font
            .get_glyphs_for_characters(&unichars[..len], &mut glyphs[..len])
        {
            return None;
        }

        // We can have pairs due to chars like emoji but we expect all of them
        // to decode down into exactly one glyph ID.
        if pair {
            assert(glyphs[1] == 0);
        }

        Some(glyphs[0] as u32)
    }

    pub fn render_glyph(
        &self,
        atlas: &mut Atlas,
        glyph_index: u32,
        opts: RenderOptions,
    ) -> Result<Glyph, Error> {
        let glyphs = [glyph_index as macos::graphics::Glyph];

        // Get the bounding rect for rendering this glyph.
        // This is in a coordinate space with (0.0, 0.0)
        // in the bottom left and +Y pointing up.
        let mut rect = self.font.get_bounding_rects_for_glyphs(
            macos::text::FontOrientation::Horizontal,
            &glyphs,
            None,
        );

        // Determine whether this is a color glyph.
        let is_color = self.is_color_glyph(glyph_index);
        // And whether it's (probably) a bitmap (sbix).
        let sbix = is_color && self.color.as_ref().is_some_and(|c| c.sbix);

        // If we're rendering a synthetic bold then we will gain 50% of
        // the line width on every edge, which means we should increase
        // our width and height by the line width and subtract half from
        // our origin points.
        //
        // We don't add extra size if it's a sbix color font though,
        // since bitmaps aren't affected by synthetic bold.
        if !sbix {
            if let Some(line_width) = self.synthetic_bold {
                rect.size.width += line_width;
                rect.size.height += line_width;
                rect.origin.x -= line_width / 2.0;
                rect.origin.y -= line_width / 2.0;
            }
        }

        // If our rect is smaller than a quarter pixel in either axis
        // then it has no outlines or they're too small to render.
        //
        // In this case we just return 0-sized glyph struct.
        if rect.size.width < 0.25 || rect.size.height < 0.25 {
            return Ok(Glyph {
                width: 0,
                height: 0,
                offset_x: 0,
                offset_y: 0,
                atlas_x: 0,
                atlas_y: 0,
            });
        }

        let metrics = opts.grid_metrics;
        let cell_width = metrics.cell_width as f64;
        let cell_height = metrics.cell_height as f64;

        // Next we apply any constraints to get the final size of the glyph.
        let constraint = opts.constraint;

        // We need to add the baseline position before passing to the constrain
        // function since it operates on cell-relative positions, not baseline.
        let cell_baseline = metrics.cell_baseline as f64;

        let glyph_size = constraint.constrain(
            glyph::Size {
                width: rect.size.width,
                height: rect.size.height,
                x: rect.origin.x,
                y: rect.origin.y + cell_baseline,
            },
            metrics,
            opts.constraint_width,
        );

        let mut x = glyph_size.x;
        let mut y = glyph_size.y;
        let mut width = glyph_size.width;
        let mut height = glyph_size.height;

        // We center all glyphs within the pixel-rounded and adjusted
        // cell width if it's larger than the face width, so that they
        // aren't weirdly off to the left.
        //
        // We don't do this if the glyph has a stretch constraint,
        // since in that case the position was already calculated with the
        // new cell width in mind.
        if constraint.size != ConstraintSize::Stretch {
            // We add half the difference to re-center.
            let dx = (cell_width - metrics.face_width) / 2.0;
            x += dx;
            if dx < 0.0 {
                // For negative diff (cell narrower than advance), we remove the
                // integer part and only keep the fractional adjustment needed
                // for consistent subpixel positioning.
                x -= dx.trunc();
            }
        }

        // If this is a bitmap glyph, it will always render as full pixels,
        // not fractional pixels, so we need to quantize its position and
        // size accordingly to align to full pixels so we get good results.
        if sbix {
            width = cell_width - (cell_width - width - x).round() - x.round();
            height = cell_height - (cell_height - height - y).round() - y.round();
            x = x.round();
            y = y.round();
        }

        // We make an assumption that font smoothing ("thicken")
        // adds no more than 1 extra pixel to any edge. We don't
        // add extra size if it's a sbix color font though, since
        // bitmaps aren't affected by smoothing.
        let canvas_padding: u32 = if opts.thicken && !sbix { 1 } else { 0 };

        // Our whole-pixel bearings for the final glyph.
        // The fractional portion will be included in the rasterized position.
        let px_x = x.floor() as i32 - canvas_padding as i32;
        let px_y = y.floor() as i32 - canvas_padding as i32;

        // We keep track of the fractional part of the pixel bearings, which
        // we will add as an offset when rasterizing to make sure we get the
        // correct sub-pixel position.
        let frac_x = x - x.floor();
        let frac_y = y - y.floor();

        // Add the fractional pixel to the width and height and take
        // the ceiling to get a canvas size that will definitely fit
        // our drawn glyph, including the fractional offset and font smoothing.
        let px_width = (width + frac_x).ceil() as u32 + (2 * canvas_padding);
        let px_height = (height + frac_y).ceil() as u32 + (2 * canvas_padding);

        // Settings that are specific to if we are rendering text or emoji.
        struct ColorSettings {
            color: bool,
            depth: u32,
            space: &'static macos::graphics::ColorSpace,
            context_opts: u32,
        }
        let color = if !is_color {
            ColorSettings {
                color: false,
                depth: 1,
                space: macos::graphics::ColorSpace::create_named(
                    macos::graphics::color_space::Name::LinearGray,
                )?,
                context_opts: macos::graphics::ImageAlphaInfo::Only as u32,
            }
        } else {
            ColorSettings {
                color: true,
                depth: 4,
                space: macos::graphics::ColorSpace::create_named(
                    macos::graphics::color_space::Name::DisplayP3,
                )?,
                context_opts: macos::graphics::BitmapInfo::BYTE_ORDER_32_LITTLE.0
                    | macos::graphics::ImageAlphaInfo::PremultipliedFirst as u32,
            }
        };
        let _space_release = Release(color.space);

        // This is just a safety check.
        if atlas.format.depth() as u32 != color.depth {
            log::warn!(
                target: LOG,
                "font atlas color depth doesn't equal font color depth atlas={} font={}",
                atlas.format.depth(),
                color.depth,
            );
            return Err(Error::InvalidAtlasFormat);
        }

        // Our buffer for rendering. We could cache this but glyph rasterization
        // usually stabilizes pretty quickly and is very infrequent so I think
        // the allocation overhead is acceptable compared to the cost of
        // caching it forever or having to deal with a cache lifetime.
        let mut buf = vec![0u8; (px_width * px_height * color.depth) as usize];

        let ctx = macos::graphics::BitmapContext::create(
            Some(&mut buf),
            px_width as usize,
            px_height as usize,
            8,
            (px_width * color.depth) as usize,
            color.space,
            color.context_opts,
        )?;
        {
            use macos::graphics::Context;

            // Perform an initial fill. This ensures that we don't have any
            // uninitialized pixels in the bitmap.
            if color.color {
                Context::set_rgb_fill_color(ctx, 0.0, 0.0, 0.0, 0.0);
            } else {
                Context::set_gray_fill_color(ctx, 0.0, 0.0);
            }
            Context::fill_rect(
                ctx,
                macos::graphics::Rect {
                    origin: macos::graphics::Point { x: 0.0, y: 0.0 },
                    size: macos::graphics::Size {
                        width: px_width as f64,
                        height: px_height as f64,
                    },
                },
            );

            // "Font smoothing" is what we call "thickening", it's an attempt
            // to compensate for optical thinning of fonts, but at this point
            // it's just something that makes the text look closer to system
            // applications if users want that.
            Context::set_allows_font_smoothing(ctx, true);
            Context::set_should_smooth_fonts(ctx, opts.thicken);

            // Subpixel positioning allows glyphs to be placed at non-integer
            // coordinates. We need this for our alignment.
            Context::set_allows_font_subpixel_positioning(ctx, true);
            Context::set_should_subpixel_position_fonts(ctx, true);

            // We don't want subpixel quantization, since we very carefully
            // manage the position of our glyphs ourselves, and dont want to
            // mess that up.
            Context::set_allows_font_subpixel_quantization(ctx, false);
            Context::set_should_subpixel_quantize_fonts(ctx, false);

            // Anti-aliasing is self explanatory.
            Context::set_allows_antialiasing(ctx, true);
            Context::set_should_antialias(ctx, true);

            // Set our color for drawing
            if color.color {
                Context::set_rgb_fill_color(ctx, 1.0, 1.0, 1.0, 1.0);
                Context::set_rgb_stroke_color(ctx, 1.0, 1.0, 1.0, 1.0);
            } else {
                let strength = opts.thicken_strength as f64;
                Context::set_gray_fill_color(ctx, strength / 255.0, 1.0);
                Context::set_gray_stroke_color(ctx, strength / 255.0, 1.0);
            }

            // If we are drawing with synthetic bold then use a fill stroke
            // which strokes the outlines of the glyph making a more bold look.
            if let Some(line_width) = self.synthetic_bold {
                Context::set_text_drawing_mode(ctx, macos::graphics::TextDrawingMode::FillStroke);
                Context::set_line_width(ctx, line_width);
            }

            // Translate our drawing context so that when we draw our
            // glyph the bottom/left edge is at the correct sub-pixel
            // position. The bottom/left edges are guaranteed to be at
            // exactly [0, 0] relative to this because when we call to
            // `drawGlyphs`, we pass the negated bearings.
            Context::translate_ctm(
                ctx,
                frac_x + canvas_padding as f64,
                frac_y + canvas_padding as f64,
            );

            // Scale the drawing context so that when we draw
            // our glyph it's stretched to the constrained size.
            Context::scale_ctm(ctx, width / rect.size.width, height / rect.size.height);

            // Draw our glyph.
            //
            // We offset the position by the negated bearings so that the
            // glyph is drawn at exactly [0, 0], which is then offset to
            // the appropriate fractional position by the translation we
            // did before scaling.
            self.font.draw_glyphs(
                &glyphs,
                &[macos::graphics::Point {
                    x: -rect.origin.x,
                    y: -rect.origin.y,
                }],
                ctx,
            );

            Context::release(ctx);
        }

        // Write our rasterized glyph to the atlas.
        let region = atlas.reserve(px_width, px_height)?;
        atlas.set(region, &buf);

        // This should be the distance from the bottom of
        // the cell to the top of the glyph's bounding box.
        let offset_y: i32 = px_y + px_height as i32;

        // This should be the distance from the left of
        // the cell to the left of the glyph's bounding box.
        let offset_x: i32 = px_x;

        Ok(Glyph {
            width: px_width,
            height: px_height,
            offset_x,
            offset_y,
            atlas_x: region.x,
            atlas_y: region.y,
        })
    }

    /// Copy a font table and parse it with `parse`, logging parse errors.
    fn parse_table<T, E: std::fmt::Debug>(
        &self,
        tags: &[&[u8; 4]],
        what: &str,
        parse: impl FnOnce(&[u8]) -> Result<T, E>,
    ) -> Option<T> {
        let data = tags
            .iter()
            .find_map(|tag| self.font.copy_table(macos::text::FontTableTag::init(tag)))?;
        let result = match parse(data.as_slice()) {
            Ok(v) => Some(v),
            Err(err) => {
                log::warn!(target: LOG, "error parsing {} table: {:?}", what, err);
                None
            }
        };
        data.release();
        result
    }

    /// Get the `FaceMetrics` for this face.
    pub fn get_metrics(&self) -> FaceMetrics {
        let ct_font = &self.font;

        // Read the 'head' table out of the font data.
        //
        // macOS bitmap-only fonts use a 'bhed' tag rather than 'head', but
        // the table format is byte-identical to the 'head' table, so if we
        // can't find 'head' we try 'bhed' instead before failing.
        //
        // ref: https://fontforge.org/docs/techref/bitmaponlysfnt.html
        let head_: Option<opentype::Head> =
            self.parse_table(&[b"head", b"bhed"], "head", opentype::Head::init);

        // Read the 'post' table out of the font data.
        let post_: Option<opentype::Post> = self.parse_table(&[b"post"], "post", opentype::Post::init);

        // Read the 'OS/2' table out of the font data if it's available.
        let os2_: Option<opentype::Os2> = self.parse_table(&[b"OS/2"], "OS/2", opentype::Os2::init);

        // Read the 'hhea' table out of the font data.
        let hhea_: Option<opentype::Hhea> = self.parse_table(&[b"hhea"], "hhea", opentype::Hhea::init);

        let units_per_em: f64 = match &head_ {
            Some(head) => head.units_per_em as f64,
            None => self.font.get_units_per_em() as f64,
        };
        let px_per_em: f64 = ct_font.get_size();
        let px_per_unit: f64 = px_per_em / units_per_em;

        let (ascent, descent, line_gap): (f64, f64, f64) = 'vertical_metrics: {
            // If we couldn't get the hhea table, rely on metrics from CoreText.
            let Some(hhea) = &hhea_ else {
                break 'vertical_metrics (
                    self.font.get_ascent(),
                    -self.font.get_descent(),
                    self.font.get_leading(),
                );
            };

            let hhea_ascent = hhea.ascender as f64;
            let hhea_descent = hhea.descender as f64;
            let hhea_line_gap = hhea.line_gap as f64;

            // If our font has no OS/2 table, then we just
            // blindly use the metrics from the hhea table.
            let Some(os2) = &os2_ else {
                break 'vertical_metrics (
                    hhea_ascent * px_per_unit,
                    hhea_descent * px_per_unit,
                    hhea_line_gap * px_per_unit,
                );
            };

            let os2_ascent = os2.s_typo_ascender as f64;
            let os2_descent = os2.s_typo_descender as f64;
            let os2_line_gap = os2.s_typo_line_gap as f64;

            // If the font says to use typo metrics, trust it.
            if os2.fs_selection.use_typo_metrics {
                break 'vertical_metrics (
                    os2_ascent * px_per_unit,
                    os2_descent * px_per_unit,
                    os2_line_gap * px_per_unit,
                );
            }

            // Otherwise we prefer the height metrics from 'hhea' if they
            // are available, or else OS/2 sTypo* metrics, and if all else
            // fails then we use OS/2 usWin* metrics.
            //
            // This is not "standard" behavior, but it's our best bet to
            // account for fonts being... just weird. It's pretty much what
            // FreeType does to get its generic ascent and descent metrics.

            if hhea.ascender != 0 || hhea.descender != 0 {
                break 'vertical_metrics (
                    hhea_ascent * px_per_unit,
                    hhea_descent * px_per_unit,
                    hhea_line_gap * px_per_unit,
                );
            }

            if os2_ascent != 0.0 || os2_descent != 0.0 {
                break 'vertical_metrics (
                    os2_ascent * px_per_unit,
                    os2_descent * px_per_unit,
                    os2_line_gap * px_per_unit,
                );
            }

            let win_ascent = os2.us_win_ascent as f64;
            let win_descent = os2.us_win_descent as f64;
            (
                win_ascent * px_per_unit,
                // usWinDescent is *positive* -> down unlike sTypoDescender
                // and hhea.Descender, so we flip its sign to fix this.
                -win_descent * px_per_unit,
                0.0,
            )
        };

        let (underline_position, underline_thickness) = match &post_ {
            None => (None, None),
            Some(post) => {
                // Some fonts have degenerate 'post' tables where the underline
                // thickness (and often position) are 0. We consider them null
                // if this is the case and use our own fallbacks when we calculate.
                let has_broken_underline = post.underline_thickness == 0;

                // If the underline position isn't 0 then we do use it,
                // even if the thickness is't properly specified.
                let pos = if has_broken_underline && post.underline_position == 0 {
                    None
                } else {
                    Some(post.underline_position as f64 * px_per_unit)
                };

                let thick = if has_broken_underline {
                    None
                } else {
                    Some(post.underline_thickness as f64 * px_per_unit)
                };

                (pos, thick)
            }
        };

        // Similar logic to the underline above.
        let (strikethrough_position, strikethrough_thickness) = match &os2_ {
            None => (None, None),
            Some(os2) => {
                let has_broken_strikethrough = os2.y_strikeout_size == 0;

                let pos = if has_broken_strikethrough && os2.y_strikeout_position == 0 {
                    None
                } else {
                    Some(os2.y_strikeout_position as f64 * px_per_unit)
                };

                let thick = if has_broken_strikethrough {
                    None
                } else {
                    Some(os2.y_strikeout_size as f64 * px_per_unit)
                };

                (pos, thick)
            }
        };

        // We fall back to whatever CoreText does if the
        // OS/2 table doesn't specify a cap or ex height.
        let (cap_height, ex_height): (f64, f64) = match &os2_ {
            None => (ct_font.get_cap_height(), ct_font.get_x_height()),
            Some(os2) => (
                match os2.s_cap_height {
                    Some(s_cap_height) => s_cap_height as f64 * px_per_unit,
                    None => ct_font.get_cap_height(),
                },
                match os2.sx_height {
                    Some(sx_height) => sx_height as f64 * px_per_unit,
                    None => ct_font.get_x_height(),
                },
            ),
        };

        // Cell width is calculated by calculating the widest width of the
        // visible ASCII characters. Usually 'M' is widest but we just take
        // whatever is widest.
        //
        // ASCII height is calculated as the height of the overall bounding
        // box of the same characters.
        let (cell_width, ascii_height): (f64, f64) = {
            // Build an array of all the ASCII chars
            const LEN: usize = 127 - 32;
            let mut unichars = [0u16; LEN];
            for (i, c) in unichars.iter_mut().enumerate() {
                *c = (i + 32) as u16;
            }

            // Get our glyph IDs for the ASCII chars
            let mut glyphs: [macos::graphics::Glyph; LEN] = [0; LEN];
            let _ = ct_font.get_glyphs_for_characters(&unichars, &mut glyphs);

            // Get all our advances
            let mut advances = [macos::graphics::Size::default(); LEN];
            let _ = ct_font.get_advances_for_glyphs(
                macos::text::FontOrientation::Horizontal,
                &glyphs,
                Some(&mut advances),
            );

            // Find the maximum advance
            let mut max: f64 = 0.0;
            for advance in &advances {
                max = advance.width.max(max);
            }

            // Get the overall bounding rect for the glyphs
            let rect = ct_font.get_bounding_rects_for_glyphs(
                macos::text::FontOrientation::Horizontal,
                &glyphs,
                None,
            );

            (max, rect.size.height)
        };

        // Measure "水" (CJK water ideograph, U+6C34) for our ic width.
        let ic_width: Option<f64> = 'ic_width: {
            let Some(glyph) = self.glyph_index('水' as u32) else {
                break 'ic_width None;
            };

            let advance = ct_font.get_advances_for_glyphs(
                macos::text::FontOrientation::Horizontal,
                &[glyph as macos::graphics::Glyph],
                None,
            );

            let bounds = ct_font.get_bounding_rects_for_glyphs(
                macos::text::FontOrientation::Horizontal,
                &[glyph as macos::graphics::Glyph],
                None,
            );

            // If the advance of the glyph is less than the width of the actual
            // glyph then we just treat it as invalid since it's probably wrong
            // and using it for size normalization will instead make the font
            // way too big.
            //
            // This can sometimes happen if there's a CJK font that has been
            // patched with the nerd fonts patcher and it butchers the advance
            // values so the advance ends up half the width of the actual glyph.
            if bounds.size.width > advance {
                let mut buf = [0u8; 1024];
                let font_name: &[u8] = self.name(&mut buf).unwrap_or(b"<Error getting font name>");
                log::warn!(
                    target: LOG,
                    "(getMetrics) Width of glyph '水' for font \"{}\" is greater than its advance ({} > {}), discarding ic_width metric.",
                    String::from_utf8_lossy(font_name),
                    bounds.size.width,
                    advance,
                );
                break 'ic_width None;
            }

            Some(advance)
        };

        FaceMetrics {
            px_per_em,

            cell_width,

            ascent,
            descent,
            line_gap,

            underline_position,
            underline_thickness,

            strikethrough_position,
            strikethrough_thickness,

            cap_height: Some(cap_height),
            ex_height: Some(ex_height),
            ascii_height: Some(ascii_height),
            ic_width,
        }
    }

    /// Copy the font table data for the given tag.
    pub fn copy_table(&self, tag: &[u8; 4]) -> Option<Vec<u8>> {
        let data = self.font.copy_table(macos::text::FontTableTag::init(tag))?;
        let buf = data.as_slice().to_vec();
        data.release();
        Some(buf)
    }
}

impl Drop for Face {
    /// Zig `deinit`.
    fn drop(&mut self) {
        self.font.release();
        if let Some(hb) = &mut self.hb_font {
            // SAFETY: we own this reference; not used afterwards.
            unsafe { hb.destroy() };
        }
        if let Some(v) = &self.color {
            v.deinit();
        }
    }
}

/// Releases a CoreFoundation object when dropped (Zig `defer x.release()`).
struct Release<'a>(&'a macos::graphics::ColorSpace);

impl Drop for Release<'_> {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// The state associated with a font face that may have colorized glyphs.
/// This is used to determine if a specific glyph ID is colorized.
#[derive(Debug)]
pub struct ColorState {
    /// True if there is an sbix font table. For now, the mere presence
    /// of an sbix font table causes us to assume the glyph is colored.
    /// We can improve this later.
    pub sbix: bool,

    /// The SVG font table data (if any), which we can use to determine
    /// if a glyph is present in the SVG table.
    ///
    /// The parsed table borrows `svg_data`, which is retained until
    /// `deinit` (hence `'static`).
    pub svg: Option<opentype::Svg<'static>>,
    pub svg_data: Option<&'static macos::foundation::Data>,
}

impl ColorState {
    pub fn init(f: &macos::text::Font) -> Result<ColorState, Error> {
        // sbix is true if the table exists in the font data at all.
        // In the future we probably want to actually parse it and
        // check for glyphs.
        let sbix = match f.copy_table(macos::text::FontTableTag::init(b"sbix")) {
            None => false,
            Some(data) => {
                let len = data.get_length();
                data.release();
                len > 0
            }
        };

        // Read the SVG table out of the font data.
        let (svg, svg_data) = match f.copy_table(macos::text::FontTableTag::init(b"SVG ")) {
            None => (None, None),
            Some(data) => {
                // SAFETY: the bytes are owned by `data`, which is retained
                // until `deinit`.
                let bytes: &'static [u8] = unsafe { std::mem::transmute(data.as_slice()) };
                match opentype::Svg::init(bytes) {
                    Ok(svg) => (Some(svg), Some(data)),
                    Err(_) => {
                        data.release();
                        return Err(Error::InvalidSVGTable);
                    }
                }
            }
        };

        Ok(ColorState {
            sbix,
            svg,
            svg_data,
        })
    }

    pub fn deinit(&self) {
        if let Some(v) = &self.svg_data {
            v.release();
        }
    }

    /// Returns true if the given glyph ID is colored.
    pub fn is_color_glyph(&self, glyph_id: u32) -> bool {
        // Our font system uses 32-bit glyph IDs for special values but
        // actual fonts only contain 16-bit glyph IDs so if we can't cast
        // into it it must be false.
        let Ok(glyph_u16) = u16::try_from(glyph_id) else {
            return false;
        };

        // sbix is always true for now
        if self.sbix {
            return true;
        }

        // if we have svg data, check it
        if let Some(svg) = &self.svg {
            if svg.has_glyph(glyph_u16) {
                return true;
            }
        }

        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::font::embedded;
    use crate::font::metrics::Metrics;

    fn ct_font_named(name: &str) -> &'static macos::text::Font {
        let name = macos::foundation::String::create_with_bytes(
            name.as_bytes(),
            macos::foundation::StringEncoding::Utf8,
            false,
        )
        .unwrap();
        let desc = macos::text::FontDescriptor::create_with_name_and_size(name, 12.0).unwrap();
        let ct_font = macos::text::Font::create_with_font_descriptor(desc, 12.0).unwrap();
        desc.release();
        name.release();
        ct_font
    }

    fn render_all_ascii(face: &Face, atlas: &mut Atlas) {
        // Generate all visible ASCII
        for i in 32u8..127 {
            assert!(face.glyph_index(i as u32).is_some());
            face.render_glyph(
                atlas,
                face.glyph_index(i as u32).unwrap(),
                RenderOptions::new(Metrics::calc(face.get_metrics())),
            )
            .unwrap();
        }
    }

    #[test]
    fn render_ascii() {
        // Zig: unnamed `test` block.
        let mut atlas = Atlas::init(512, atlas::Format::Grayscale).unwrap();

        let ct_font = ct_font_named("Monaco");
        let face = Face::init_font_copy(ct_font, Options::new(DesiredSize::new(12.0))).unwrap();
        ct_font.release();

        render_all_ascii(&face, &mut atlas);
    }

    #[test]
    fn name() {
        let ct_font = ct_font_named("Menlo");
        let face = Face::init_font_copy(ct_font, Options::new(DesiredSize::new(12.0))).unwrap();
        ct_font.release();

        let mut buf = [0u8; 1024];
        let font_name = face.name(&mut buf).unwrap();
        assert_eq!(font_name, b"Menlo");
    }

    #[test]
    fn emoji() {
        let ct_font = ct_font_named("Apple Color Emoji");
        let face = Face::init_font_copy(ct_font, Options::new(DesiredSize::new(18.0))).unwrap();
        ct_font.release();

        // Glyph index check
        {
            let id = face.glyph_index('🥸' as u32).unwrap();
            assert!(face.is_color_glyph(id));
        }
    }

    #[test]
    fn in_memory() {
        let test_font = embedded::REGULAR;

        let mut atlas = Atlas::init(512, atlas::Format::Grayscale).unwrap();

        let lib = Library::init().unwrap();

        let face = Face::init(lib, test_font, Options::new(DesiredSize::new(12.0))).unwrap();

        render_all_ascii(&face, &mut atlas);
    }

    #[test]
    fn variable() {
        let test_font = embedded::VARIABLE;

        let mut atlas = Atlas::init(512, atlas::Format::Grayscale).unwrap();

        let lib = Library::init().unwrap();

        let face = Face::init(lib, test_font, Options::new(DesiredSize::new(12.0))).unwrap();

        render_all_ascii(&face, &mut atlas);
    }

    #[test]
    fn variable_set_variation() {
        let test_font = embedded::VARIABLE;

        let mut atlas = Atlas::init(512, atlas::Format::Grayscale).unwrap();

        let lib = Library::init().unwrap();

        let mut face = Face::init(lib, test_font, Options::new(DesiredSize::new(12.0))).unwrap();

        face.set_variations(
            &[Variation {
                id: VariationId::init(b"wght"),
                value: 400.0,
            }],
            Options::new(DesiredSize::new(12.0)),
        )
        .unwrap();

        render_all_ascii(&face, &mut atlas);
    }

    #[test]
    fn svg_font_table() {
        let test_font = embedded::JULIA_MONO;

        let lib = Library::init().unwrap();

        let face = Face::init(lib, test_font, Options::new(DesiredSize::new(12.0))).unwrap();

        let table = face.copy_table(b"SVG ").unwrap();

        assert!(!table.is_empty());
    }

    #[test]
    fn glyph_index_colored_vs_text() {
        let test_font = embedded::JULIA_MONO;

        let lib = Library::init().unwrap();

        let face = Face::init(lib, test_font, Options::new(DesiredSize::new(12.0))).unwrap();

        {
            let glyph = face.glyph_index('A' as u32).unwrap();
            assert_eq!(4, glyph);
            assert!(!face.is_color_glyph(glyph));
        }

        {
            let glyph = face.glyph_index(0xE800).unwrap();
            assert_eq!(11482, glyph);
            assert!(face.is_color_glyph(glyph));
        }
    }
}
