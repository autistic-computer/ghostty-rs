//! Port of `src/font/face/freetype.zig`.
//!
//! STATUS: ported
//!
//! Face represents a single font face. A single font face has a single set
//! of properties associated with it such as style, weight, etc.
//!
//! A Face isn't typically meant to be used directly. It is usually used
//! via a Family in order to store it in an Atlas.

use std::sync::{Mutex, MutexGuard};

use crate::font::atlas::{self, Atlas};
use crate::font::face::{DesiredSize, FreetypeLoadFlags, NameError, Options, Variation, VariationId};
use crate::font::glyph::{self, ConstraintSize, Glyph, RenderOptions};
use crate::font::library::FreetypeLibrary as Library;
use crate::font::metrics::FaceMetrics;
use crate::font::opentype::sfnt::F26Dot6;
use crate::pkg::freetype;
use crate::pkg::freetype::c;
use crate::pkg::harfbuzz;
use crate::quirks;
use crate::stb;

const LOG: &str = "font_face";

/// Errors from initializing a face or rendering a glyph (the Zig inferred
/// error sets of `Face.init*` and `Face.renderGlyph`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// A FreeType call failed.
    Freetype(freetype::Error),
    /// A HarfBuzz call failed.
    Harfbuzz(harfbuzz::Error),
    OutOfMemory,
    /// The atlas is full and must be grown (see [`atlas::Error`]).
    AtlasFull,
    BitmapHandlingError,
    GlyphResizeFailed,
    UnsupportedGlyphFormat,
    WrongAtlas,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Freetype(e) => write!(f, "{e}"),
            Error::Harfbuzz(e) => write!(f, "{e}"),
            other => std::fmt::Debug::fmt(other, f),
        }
    }
}

impl std::error::Error for Error {}

impl From<freetype::Error> for Error {
    fn from(e: freetype::Error) -> Error {
        Error::Freetype(e)
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

/// Zig: the anonymous `packed struct { italic, bold }` of `Face.synthetic`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Synthetic {
    pub italic: bool,
    pub bold: bool,
}

pub struct Face {
    /// Our Library
    pub lib: Library,

    /// Our font face.
    pub face: freetype::Face,

    /// This mutex MUST be held while doing anything with the
    /// glyph slot on the freetype face, because this struct
    /// may be shared across multiple surfaces.
    ///
    /// This means that anywhere where `self.face.loadGlyph`
    /// is called, this mutex must be held.
    pub ft_mutex: Box<Mutex<()>>,

    /// Harfbuzz font corresponding to this face.
    pub hb_font: harfbuzz::Font,

    /// Freetype load flags for this font face.
    pub load_flags: FreetypeLoadFlags,

    /// Set quirks.disableDefaultFontFeatures
    pub quirks_disable_default_font_features: bool,

    /// Set to true to apply a synthetic italic to the face.
    pub synthetic: Synthetic,

    /// The current size this font is set to.
    pub size: DesiredSize,
}

impl std::fmt::Debug for Face {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Face")
            .field("face", &self.face)
            .field("load_flags", &self.load_flags)
            .field("synthetic", &self.synthetic)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

// SAFETY: like the Zig struct (which is shared between threads by the
// shared grid), every use of the FreeType glyph slot is serialized by
// `ft_mutex`, and face creation/destruction by the library mutex. The
// remaining FreeType/HarfBuzz calls only read face data.
unsafe impl Send for Face {}
unsafe impl Sync for Face {}

/// Lock a mutex, ignoring poisoning (Zig mutexes have no poisoning).
fn lock(m: &Mutex<()>) -> MutexGuard<'_, ()> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Face {
    /// Initialize a new font face with the given source in-memory.
    pub fn init_file(
        lib: Library,
        path: &std::ffi::CStr,
        index: i32,
        opts: Options,
    ) -> Result<Face, Error> {
        let face = {
            let _guard = lock(&lib.mutex);
            lib.lib.init_face(path, index)?
        };
        Self::init_face_or_release(lib, face, opts)
    }

    /// Initialize a new font face with the given source in-memory.
    ///
    /// FreeType does not copy the font data, so (like the Zig version,
    /// which requires the caller to keep it alive) the source must outlive
    /// the face; this port requires `'static` data (e.g. the embedded
    /// fonts).
    pub fn init(lib: Library, source: &'static [u8], opts: Options) -> Result<Face, Error> {
        let face = {
            let _guard = lock(&lib.mutex);
            // SAFETY: `source` is 'static so it outlives the face.
            unsafe { lib.lib.init_memory_face(source, 0)? }
        };
        Self::init_face_or_release(lib, face, opts)
    }

    /// `initFace` plus the callers' `errdefer face.deinit()`.
    fn init_face_or_release(
        lib: Library,
        face: freetype::Face,
        opts: Options,
    ) -> Result<Face, Error> {
        match Self::init_face(lib.clone(), face, opts) {
            Ok(v) => Ok(v),
            Err(err) => {
                let _guard = lock(&lib.mutex);
                // SAFETY: we own this reference to the face and it is not
                // used afterwards.
                unsafe { face.deinit() };
                Err(err)
            }
        }
    }

    /// On error the caller still owns `face` (Zig `errdefer face.deinit()`
    /// in the callers).
    fn init_face(lib: Library, face: freetype::Face, opts: Options) -> Result<Face, Error> {
        face.select_charmap(freetype::face::Encoding::Unicode)?;
        Self::set_size_(face, opts.size)?;

        // SAFETY: valid face handle; HarfBuzz takes its own reference.
        let hb_font = unsafe { harfbuzz::freetype::create_font(face.handle)? };

        let mut result = Face {
            lib,
            face,
            hb_font,
            ft_mutex: Box::new(Mutex::new(())),
            load_flags: opts.freetype_load_flags,
            quirks_disable_default_font_features: false,
            synthetic: Synthetic::default(),
            size: opts.size,
        };
        result.quirks_disable_default_font_features =
            quirks::disable_default_font_features(&result);

        // In debug mode, we output information about available variation axes,
        // if they exist.
        if cfg!(debug_assertions) && face.has_multiple_masters() {
            let mut buf = [0u8; 1024];
            let name = String::from_utf8_lossy(result.name(&mut buf).unwrap_or(b"")).into_owned();
            log::debug!(target: LOG, "variation axes font={}", name);

            // On error the result is dropped, which releases the face; but
            // the caller still owns `face` on error, so take a reference
            // for the drop.
            let mm = match face.get_mm_var() {
                Ok(mm) => mm,
                Err(err) => {
                    face.r#ref();
                    drop(result);
                    return Err(err.into());
                }
            };
            // SAFETY: `mm` is a valid FT_MM_Var with `num_axis` axes.
            unsafe {
                for i in 0..(*mm).num_axis as usize {
                    let axis = *(*mm).axis.add(i);
                    let Ok(id_raw) = i32::try_from(axis.tag) else {
                        continue;
                    };
                    let id = VariationId::from_bits(id_raw as u32);
                    let axis_name = if axis.name.is_null() {
                        std::borrow::Cow::Borrowed("")
                    } else {
                        std::ffi::CStr::from_ptr(axis.name).to_string_lossy()
                    };
                    log::debug!(
                        target: LOG,
                        "variation axis: name={} id={} min={} max={} def={}",
                        axis_name,
                        String::from_utf8_lossy(&id.str()),
                        axis.minimum >> 16,
                        axis.maximum >> 16,
                        axis.def >> 16,
                    );
                }
                result.lib.lib.done_mm_var(mm);
            }
        }

        Ok(result)
    }

    /// Returns the font name. If allocation is required, buf will be used,
    /// but sometimes allocation isn't required and a static string is
    /// returned.
    pub fn name<'a>(&'a self, buf: &'a mut [u8]) -> Result<&'a [u8], NameError> {
        let count = self.face.get_sfnt_name_count();

        // We look for the font family entry.
        for i in 0..count {
            let Ok(entry) = self.face.get_sfnt_name(i) else {
                continue;
            };
            if entry.name_id as i32 == c::TT_NAME_ID_FONT_FAMILY {
                let string: &'a [u8] = if entry.string.is_null() {
                    &[]
                } else {
                    // SAFETY: FreeType guarantees `string` points to
                    // `string_len` bytes owned by the face.
                    unsafe { std::slice::from_raw_parts(entry.string, entry.string_len as usize) }
                };
                // There are other encodings that are something other than UTF-8
                // but this is one we've seen "in the wild" so far.
                if entry.platform_id as i32 == c::TT_PLATFORM_MICROSOFT
                    && entry.encoding_id as i32 == c::TT_MS_ID_UNICODE_CS
                    && string.len() % 2 == 0
                    && string.len() <= 1024
                {
                    // The string is UTF-16BE.
                    let mut tmp = [0u16; 512];
                    let max = string.len() / 2;
                    for (j, c) in string.chunks_exact(2).enumerate() {
                        tmp[j] = u16::from_be_bytes([c[0], c[1]]);
                    }
                    let Ok(decoded) = String::from_utf16(&tmp[..max]) else {
                        return Ok(string);
                    };
                    // NOTE: Zig's utf16LeToUtf8 doesn't check the buffer
                    // size; we fall back to the raw string instead.
                    if decoded.len() > buf.len() {
                        return Ok(string);
                    }
                    buf[..decoded.len()].copy_from_slice(decoded.as_bytes());
                    return Ok(&buf[..decoded.len()]);
                }
                return Ok(string);
            }
        }

        Ok(b"")
    }

    /// Return a new face that is the same as this but also has synthetic
    /// bold applied.
    pub fn synthetic_bold(&self, opts: Options) -> Result<Face, Error> {
        // Increase face ref count
        self.face.r#ref();
        let mut f = Self::init_face_or_release(self.lib.clone(), self.face, opts)?;
        f.synthetic = self.synthetic;
        f.synthetic.bold = true;

        Ok(f)
    }

    /// Return a new face that is the same as this but has a transformation
    /// matrix applied to italicize it.
    pub fn synthetic_italic(&self, opts: Options) -> Result<Face, Error> {
        // Increase face ref count
        self.face.r#ref();
        let mut f = Self::init_face_or_release(self.lib.clone(), self.face, opts)?;
        f.synthetic = self.synthetic;
        f.synthetic.italic = true;

        Ok(f)
    }

    /// Resize the font in-place. If this succeeds, the caller is responsible
    /// for clearing any glyph caches, font atlas data, etc.
    pub fn set_size(&mut self, opts: Options) -> Result<(), freetype::Error> {
        Self::set_size_(self.face, opts.size)?;
        self.size = opts.size;
        Ok(())
    }

    fn set_size_(face: freetype::Face, size: DesiredSize) -> Result<(), freetype::Error> {
        // If we have fixed sizes, we just have to try to pick the one closest
        // to what the user requested. Otherwise, we can choose an arbitrary
        // pixel size.
        if face.is_scalable() {
            let size_26dot6 = (size.points * 64.0).round() as i32;
            face.set_char_size(0, size_26dot6, size.xdpi, size.ydpi)
        } else {
            Self::select_size_nearest(face, size.pixels().round() as u32)
        }
    }

    /// Selects the fixed size in the loaded face that is closest to the
    /// requested pixel size.
    fn select_size_nearest(face: freetype::Face, size: u32) -> Result<(), freetype::Error> {
        let mut i: i32 = 0;
        let mut best_i: i32 = 0;
        let mut best_diff: i32 = 0;
        // SAFETY: valid face handle; `available_sizes` has
        // `num_fixed_sizes` entries.
        unsafe {
            while i < (*face.handle).num_fixed_sizes {
                let width = (*(*face.handle).available_sizes.add(i as usize)).width;
                let diff = size as i32 - width as i32;
                if i == 0 || diff < best_diff {
                    best_diff = diff;
                    best_i = i;
                }
                i += 1;
            }
        }

        face.select_size(best_i)
    }

    /// Set the variation axes for this font. This will modify this font
    /// in-place.
    pub fn set_variations(&mut self, vs: &[Variation], opts: Options) -> Result<(), freetype::Error> {
        let _ = opts;

        // If this font doesn't support variations, we can't do anything.
        if !self.face.has_multiple_masters() || vs.is_empty() {
            return Ok(());
        }

        // Freetype requires that we send ALL coordinates in at once so the
        // first thing we have to do is get all the vars and put them into
        // an array.
        let mm = self.face.get_mm_var()?;
        struct DoneMmVar(freetype::Library, *mut c::FT_MM_Var);
        impl Drop for DoneMmVar {
            fn drop(&mut self) {
                // SAFETY: the MM var came from a face of this library and
                // is not used after this.
                unsafe { self.0.done_mm_var(self.1) };
            }
        }
        let _done = DoneMmVar(self.lib.lib, mm);

        // SAFETY: `mm` is a valid FT_MM_Var until `_done` is dropped.
        let (num_axis, axes) = unsafe { ((*mm).num_axis as usize, (*mm).axis) };

        // To avoid allocations, we cap the number of variation axes we can
        // support. This is arbitrary but Firefox caps this at 16 so I
        // feel like that's probably safe... and we do double cause its
        // cheap.
        let mut coords_buf: [c::FT_Fixed; 32] = [0; 32];
        let coords_len = coords_buf.len().min(num_axis);
        let coords = &mut coords_buf[..coords_len];
        self.face.get_var_design_coordinates(coords)?;

        // Now we go through each axis and see if its set. This is slow
        // but there usually aren't many axes and usually not many set
        // variations, either.
        for i in 0..num_axis {
            // SAFETY: i < num_axis.
            let axis = unsafe { *axes.add(i) };
            let Ok(id) = u32::try_from(axis.tag) else {
                continue;
            };
            for v in vs {
                if id == v.id.to_bits() {
                    coords[i] = (v.value * 65536.0) as c::FT_Fixed;
                    break;
                }
            }
        }

        // Set them!
        self.face.set_var_design_coordinates(coords)
    }

    /// Returns the glyph index for the given Unicode code point. If this
    /// face doesn't support this glyph, null is returned.
    pub fn glyph_index(&self, cp: u32) -> Option<u32> {
        self.face.get_char_index(cp)
    }

    /// Returns true if this font is colored. This can be used by callers to
    /// determine what kind of atlas to pass in.
    pub fn has_color(&self) -> bool {
        self.face.has_color()
    }

    /// Returns true if the given glyph ID is colorized.
    pub fn is_color_glyph(&self, glyph_id: u32) -> bool {
        let _guard = lock(&self.ft_mutex);

        // Load the glyph and see what pixel mode it renders with.
        // All modes other than BGRA are non-color.
        // If the glyph fails to load, just return false.
        if self
            .face
            .load_glyph(
                glyph_id,
                freetype::LoadFlags {
                    render: true,
                    color: self.face.has_color(),
                    // NO_SVG set to true because we don't currently support rendering
                    // SVG glyphs under FreeType, since that requires bundling another
                    // dependency to handle rendering the SVG.
                    no_svg: true,
                    ..Default::default()
                },
            )
            .is_err()
        {
            return false;
        }

        // SAFETY: valid face; the glyph slot is guarded by `ft_mutex`.
        unsafe {
            let glyph = (*self.face.handle).glyph;
            (*glyph).bitmap.pixel_mode as u32 == c::FT_PIXEL_MODE_BGRA
        }
    }

    /// Set the load flags to use when loading a glyph for measurement or
    /// rendering.
    fn glyph_load_flags(&self, constrained: bool) -> freetype::LoadFlags {
        // Hinting should only be enabled if the configured load flags specify
        // it and the provided constraint doesn't actually do anything, since
        // if it does, then it'll mess up the hinting anyway when it moves or
        // resizes the glyph.
        let do_hinting = self.load_flags.hinting && !constrained;

        freetype::LoadFlags {
            // If our glyph has color, we want to render the color
            color: self.face.has_color(),

            // We don't render, because we'll invoke the render
            // manually after applying constraints further down.
            render: false,

            // use options from config
            no_hinting: !do_hinting,
            force_autohint: self.load_flags.force_autohint,
            no_autohint: !self.load_flags.autohint,

            // If we're gonna be rendering this glyph in monochrome,
            // then we should use the monochrome hinter as well, or
            // else it won't look very good at all.
            //
            // Otherwise if the user asked for light hinting we
            // use that, otherwise we just use the normal target.
            target: if self.load_flags.monochrome {
                freetype::face::LoadTarget::Mono
            } else if self.load_flags.light {
                freetype::face::LoadTarget::Light
            } else {
                freetype::face::LoadTarget::Normal
            },

            // NO_SVG set to true because we don't currently support rendering
            // SVG glyphs under FreeType, since that requires bundling another
            // dependency to handle rendering the SVG.
            no_svg: true,

            ..Default::default()
        }
    }

    /// Get a rect that represents the position and size of the loaded glyph.
    ///
    /// # Safety
    /// `glyph` must be a valid, loaded glyph slot.
    unsafe fn get_glyph_size(glyph: c::FT_GlyphSlot) -> glyph::Size {
        // If we're dealing with an outline glyph then we get the
        // outline's bounding box instead of using the built-in
        // metrics, since that's more precise and allows better
        // cell-fitting.
        if (*glyph).format == c::FT_GLYPH_FORMAT_OUTLINE {
            // Get the glyph's bounding box before we transform it at all.
            // We use this rather than the metrics, since it's more precise.
            let mut bbox: c::FT_BBox = std::mem::zeroed();
            let _ = c::FT_Outline_Get_BBox(&mut (*glyph).outline, &mut bbox);

            return glyph::Size {
                x: f26dot6_to_f64(bbox.xMin),
                y: f26dot6_to_f64(bbox.yMin),
                width: f26dot6_to_f64(bbox.xMax - bbox.xMin),
                height: f26dot6_to_f64(bbox.yMax - bbox.yMin),
            };
        }

        let m = (*glyph).metrics;
        glyph::Size {
            x: f26dot6_to_f64(m.horiBearingX),
            y: f26dot6_to_f64(m.horiBearingY - m.height),
            width: f26dot6_to_f64(m.width),
            height: f26dot6_to_f64(m.height),
        }
    }

    /// Render a glyph using the glyph index. The rendered glyph is stored in the
    /// given texture atlas.
    pub fn render_glyph(
        &self,
        atlas: &mut Atlas,
        glyph_index: u32,
        opts: RenderOptions,
    ) -> Result<Glyph, Error> {
        let _guard = lock(&self.ft_mutex);

        // Load the glyph.
        self.face
            .load_glyph(glyph_index, self.glyph_load_flags(opts.constraint.does_anything()))?;

        // SAFETY: valid face handle; the glyph slot is guarded by
        // `ft_mutex` for the rest of this function.
        unsafe { self.render_loaded_glyph(atlas, glyph_index, opts) }
    }

    /// The part of `renderGlyph` after loading the glyph.
    ///
    /// # Safety
    /// The glyph slot must hold the loaded glyph and `ft_mutex` must be
    /// held.
    unsafe fn render_loaded_glyph(
        &self,
        atlas: &mut Atlas,
        glyph_index: u32,
        opts: RenderOptions,
    ) -> Result<Glyph, Error> {
        let ft_lib = self.lib.lib.handle;
        let glyph = (*self.face.handle).glyph;

        // For synthetic bold, we embolden the glyph.
        if self.synthetic.bold {
            // We need to scale the embolden amount based on the font size.
            // This is a heuristic I found worked well across a variety of
            // founts: 1 pixel per 64 units of height.
            let font_height = (*(*self.face.handle).size).metrics.height as f64;
            let ratio: f64 = 64.0 / 2048.0;
            let amount = (font_height * ratio).ceil();
            let _ = c::FT_Outline_Embolden(&mut (*glyph).outline, amount as c::FT_Pos);
        }

        // We get a rect that represents the position
        // and size of the glyph before constraints.
        let rect = Self::get_glyph_size(glyph);

        // If our glyph is smaller than a quarter pixel in either axis
        // then it has no outlines or they're too small to render.
        //
        // In this case we just return 0-sized glyph struct.
        if rect.width < 0.25 || rect.height < 0.25 {
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
                width: rect.width,
                height: rect.height,
                x: rect.x,
                y: rect.y + cell_baseline,
            },
            metrics,
            opts.constraint_width,
        );

        let mut width = glyph_size.width;
        let mut height = glyph_size.height;
        let mut x = glyph_size.x;
        let mut y = glyph_size.y;

        // We center all glyphs within the pixel-rounded and adjusted
        // cell width if it's larger than the face width, so that they
        // aren't weirdly off to the left.
        //
        // We don't do this if the glyph has a stretch constraint,
        // since in that case the position was already calculated with the
        // new cell width in mind.
        if constraint.size != ConstraintSize::Stretch && metrics.face_width < cell_width {
            // We add half the difference to re-center.
            //
            // NOTE: We round this to a whole-pixel amount because under
            //       FreeType, the outlines will be hinted, which isn't
            //       the case under CoreText. If we move the outlines by
            //       a non-whole-pixel amount, it completely ruins the
            //       hinting.
            x += ((cell_width - metrics.face_width) / 2.0).round();
        }

        // If this is a bitmap glyph, it will always render as full pixels,
        // not fractional pixels, so we need to quantize its position and
        // size accordingly to align to full pixels so we get good results.
        if (*glyph).format == c::FT_GLYPH_FORMAT_BITMAP {
            width = cell_width - (cell_width - width - x).round() - x.round();
            height = cell_height - (cell_height - height - y).round() - y.round();
            x = x.round();
            y = y.round();
        }

        // Now we can render the glyph.
        struct BitmapGuard {
            lib: c::FT_Library,
            bitmap: c::FT_Bitmap,
        }
        impl Drop for BitmapGuard {
            fn drop(&mut self) {
                // SAFETY: the bitmap was initialized with FT_Bitmap_Init
                // and is owned by this guard.
                unsafe {
                    let _ = c::FT_Bitmap_Done(self.lib, &mut self.bitmap);
                }
            }
        }
        let mut guard = BitmapGuard {
            lib: ft_lib,
            bitmap: std::mem::zeroed(),
        };
        c::FT_Bitmap_Init(&mut guard.bitmap);
        let bitmap = &mut guard.bitmap;

        match (*glyph).format {
            c::FT_GLYPH_FORMAT_OUTLINE => {
                // Manually adjust the glyph outline with this transform.
                //
                // This offers better precision than using the freetype transform
                // matrix, since that has 16.16 coefficients, and also I was having
                // weird issues that I can only assume where due to freetype doing
                // some bad caching or something when I did this using the matrix.
                let scale_x = width / rect.width;
                let scale_y = height / rect.height;
                let skew: f64 = if self.synthetic.italic {
                    // We skew by 12 degrees to synthesize italics.
                    12.0f64.to_radians().tan()
                } else {
                    0.0
                };

                let outline: &mut c::FT_Outline = &mut (*glyph).outline;
                let n_points = outline.n_points as usize;
                if n_points > 0 {
                    let points = std::slice::from_raw_parts_mut(outline.points, n_points);
                    for p in points {
                        // Convert to f64 for processing
                        let mut px = f26dot6_to_f64(p.x);
                        let mut py = f26dot6_to_f64(p.y);

                        // Subtract original bearings
                        px -= rect.x;
                        py -= rect.y;

                        // Scale
                        px *= scale_x;
                        py *= scale_y;

                        // Add new bearings
                        px += x;
                        py += y - cell_baseline;

                        // Skew
                        px += py * skew;

                        // Convert back and store
                        p.x = F26Dot6::from(px).to_bits() as c::FT_Pos;
                        p.y = F26Dot6::from(py).to_bits() as c::FT_Pos;
                    }
                }

                self.face.render_glyph(if self.load_flags.monochrome {
                    freetype::face::RenderMode::Mono
                } else {
                    freetype::face::RenderMode::Normal
                })?;

                // Copy the glyph's bitmap, making sure
                // that it's 8bpp and densely packed.
                if c::FT_Bitmap_Convert(ft_lib, &(*glyph).bitmap, bitmap, 1) != 0 {
                    return Err(Error::BitmapHandlingError);
                }
            }

            c::FT_GLYPH_FORMAT_BITMAP => {
                // If our glyph has a non-color bitmap, we need
                // to convert it to dense 8bpp so that the scale
                // operation works correctly.
                match (*glyph).bitmap.pixel_mode as u32 {
                    c::FT_PIXEL_MODE_BGRA | c::FT_PIXEL_MODE_GRAY => {}
                    _ => {
                        // Make sure the slot owns its bitmap,
                        // since we'll be modifying it here.
                        if c::FT_GlyphSlot_Own_Bitmap(glyph) != 0 {
                            return Err(Error::BitmapHandlingError);
                        }

                        let mut converted: c::FT_Bitmap = std::mem::zeroed();
                        c::FT_Bitmap_Init(&mut converted);
                        if c::FT_Bitmap_Convert(ft_lib, &(*glyph).bitmap, &mut converted, 1) != 0 {
                            return Err(Error::BitmapHandlingError);
                        }
                        // Free the existing glyph bitmap and
                        // replace it with the converted one.
                        let _ = c::FT_Bitmap_Done(ft_lib, &mut (*glyph).bitmap);
                        (*glyph).bitmap = converted;
                    }
                }

                let glyph_bitmap = (*glyph).bitmap;

                // Round our target width and height
                // as the size for our scaled bitmap.
                let w = width.round() as u32;
                let h = height.round() as u32;
                let pitch = w * atlas.format.depth() as u32;

                // Allocate a buffer for our scaled bitmap.
                //
                // We'll copy this to the original bitmap once we're
                // done so we can free it at the end of this scope.
                let mut buf = vec![0u8; (pitch * h) as usize];

                // Resize
                if stb::stbir_resize_uint8(
                    glyph_bitmap.buffer,
                    glyph_bitmap.width as i32,
                    glyph_bitmap.rows as i32,
                    glyph_bitmap.pitch,
                    buf.as_mut_ptr(),
                    w as i32,
                    h as i32,
                    pitch as i32,
                    atlas.format.depth() as i32,
                ) == 0
                {
                    // This should never fail because this is a
                    // fairly straightforward in-memory operation...
                    return Err(Error::GlyphResizeFailed);
                }

                let scaled_bitmap = c::FT_Bitmap {
                    buffer: buf.as_mut_ptr(),
                    width: w,
                    rows: h,
                    pitch: pitch as i32,
                    pixel_mode: glyph_bitmap.pixel_mode,
                    num_grays: glyph_bitmap.num_grays,
                    palette_mode: 0,
                    palette: std::ptr::null_mut(),
                };

                // Replace the bitmap's buffer and size info.
                if c::FT_Bitmap_Copy(ft_lib, &scaled_bitmap, bitmap) != 0 {
                    return Err(Error::BitmapHandlingError);
                }

                // Update the bearings to account for the new positioning.
                (*glyph).bitmap_top = (y - cell_baseline + height).floor() as i32;
                (*glyph).bitmap_left = x.floor() as i32;
            }

            f => {
                // Glyph formats are tags, so we can
                // output a semi-readable error here.
                let tag = f as u32;
                let s = [(tag >> 24) as u8, (tag >> 16) as u8, (tag >> 8) as u8, tag as u8];
                log::error!(
                    target: LOG,
                    "Can't render glyph with unsupported glyph format \"{}\"",
                    String::from_utf8_lossy(&s),
                );
                return Err(Error::UnsupportedGlyphFormat);
            }
        }

        // If this is a color glyph but we're trying to render it to the
        // grayscale atlas, or vice versa, then we throw and error. Maybe
        // in the future we could convert, but for now it should be fine.
        match bitmap.pixel_mode as u32 {
            c::FT_PIXEL_MODE_GRAY => {
                if atlas.format != atlas::Format::Grayscale {
                    return Err(Error::WrongAtlas);
                }
            }
            c::FT_PIXEL_MODE_BGRA => {
                if atlas.format != atlas::Format::Bgra {
                    return Err(Error::WrongAtlas);
                }
            }
            _ => {
                log::warn!(
                    target: LOG,
                    "glyph={} pixel mode={}",
                    glyph_index,
                    bitmap.pixel_mode
                );
                panic!("unsupported pixel mode");
            }
        }

        // Our whole-pixel bearings for the final glyph.
        // The fractional portion will be included in the rasterized position.
        //
        // For the Y position, FreeType's `bitmap_top` is the distance from the
        // baseline to the top of the glyph, but we need the distance from the
        // bottom of the cell to the bottom of the glyph, so first we add the
        // baseline to get the distance from the bottom of the cell to the top
        // of the glyph, then we subtract the height of the glyph to get the
        // bottom.
        let px_x: i32 = (*glyph).bitmap_left;
        let px_y: i32 = (*glyph).bitmap_top + metrics.cell_baseline as i32 - bitmap.rows as i32;

        let px_width = bitmap.width;
        let px_height = bitmap.rows;
        let len = bitmap.pitch.unsigned_abs() as usize * bitmap.rows as usize;

        // If our bitmap is grayscale, make sure to multiply all pixel
        // values by the right factor to bring `num_grays` up to 256.
        //
        // This is necessary because FT_Bitmap_Convert doesn't do this,
        // it just sets num_grays to the correct number and uses the
        // original smaller pixel values.
        if bitmap.pixel_mode as u32 == c::FT_PIXEL_MODE_GRAY && bitmap.num_grays < 256 {
            let factor = u8::try_from(255 / (bitmap.num_grays - 1)).unwrap();
            for p in std::slice::from_raw_parts_mut(bitmap.buffer, len) {
                *p *= factor;
            }
            bitmap.num_grays = 256;
        }

        // Must have non-empty bitmap because we return earlier if zero.
        // We assume the rest of this that it is non-zero so this is important.
        quirks::inline_assert(px_width > 0 && px_height > 0);

        // If this doesn't match then something is wrong.
        quirks::inline_assert(px_width as i32 * atlas.format.depth() as i32 == bitmap.pitch);

        // Allocate our texture atlas region and copy our bitmap in to it.
        let region = atlas.reserve(px_width, px_height)?;
        atlas.set(region, std::slice::from_raw_parts(bitmap.buffer, len));

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

    /// Convert 16.6 pixel format to pixels based on the scale factor of the
    /// current font size.
    #[allow(dead_code)]
    fn units_to_px_y(&self, units: i32) -> i32 {
        // SAFETY: valid face with an active size.
        let y_scale = unsafe { (*(*self.face.handle).size).metrics.y_scale };
        freetype::mul_fix(units, y_scale as i32) >> 6
    }

    /// Get the `FaceMetrics` for this face.
    pub fn get_metrics(&self) -> FaceMetrics {
        let face = self.face;

        // SAFETY: valid face with an active size.
        let size_metrics = unsafe { (*(*face.handle).size).metrics };

        // This code relies on this assumption, and it should always be
        // true since we don't do any non-uniform scaling on the font ever.
        quirks::inline_assert(size_metrics.x_ppem == size_metrics.y_ppem);

        // Read the 'head' table out of the font data.
        let head_ = face.get_sfnt_table::<c::TT_Header>();

        // Read the 'post' table out of the font data.
        let post_ = face.get_sfnt_table::<c::TT_Postscript>();

        // Read the 'OS/2' table out of the font data.
        let os2_: Option<&c::TT_OS2> = face
            .get_sfnt_table::<c::TT_OS2>()
            .filter(|os2| os2.version != 0xFFFF);

        // Read the 'hhea' table out of the font data.
        let hhea_ = face.get_sfnt_table::<c::TT_HoriHeader>();

        // Whether the font is in a scalable format. We need to know this
        // because many of the metrics provided by FreeType are invalid for
        // non-scalable fonts.
        // SAFETY: valid face handle.
        let is_scalable =
            unsafe { (*face.handle).face_flags } & c::FT_FACE_FLAG_SCALABLE as c::FT_Long != 0;

        // We get the UPM from the head table.
        //
        // If we have no head, but it is a scalable face, take the UPM from
        // FreeType's units_per_EM, otherwise we'll assume that UPM == PPEM.
        let units_per_em: c::FT_UShort = if let Some(head) = head_ {
            head.Units_Per_EM
        } else if is_scalable {
            // SAFETY: valid face handle.
            unsafe { (*face.handle).units_per_EM }
        } else {
            size_metrics.y_ppem
        };
        let px_per_em = size_metrics.y_ppem as f64;
        let px_per_unit = px_per_em / units_per_em as f64;

        let (ascent, descent, line_gap): (f64, f64, f64) = 'vertical_metrics: {
            let Some(hhea) = hhea_ else {
                // If we couldn't get the hhea table, rely on metrics from FreeType.
                let ascender = f26dot6_to_f64(size_metrics.ascender);
                let descender = f26dot6_to_f64(size_metrics.descender);
                let height = f26dot6_to_f64(size_metrics.height);
                break 'vertical_metrics (
                    ascender,
                    descender,
                    // We compute the line gap by adding the (negative) descender
                    // and subtracting the (positive) ascender from the line height
                    // to get the remaining gap size.
                    //
                    // NOTE: This might always be 0... but it doesn't hurt to do.
                    height + descender - ascender,
                );
            };

            let hhea_ascent = hhea.Ascender as f64;
            let hhea_descent = hhea.Descender as f64;
            let hhea_line_gap = hhea.Line_Gap as f64;

            // If our font has no OS/2 table, then we just
            // blindly use the metrics from the hhea table.
            let Some(os2) = os2_ else {
                break 'vertical_metrics (
                    hhea_ascent * px_per_unit,
                    hhea_descent * px_per_unit,
                    hhea_line_gap * px_per_unit,
                );
            };

            let os2_ascent = os2.sTypoAscender as f64;
            let os2_descent = os2.sTypoDescender as f64;
            let os2_line_gap = os2.sTypoLineGap as f64;

            // If the font says to use typo metrics, trust it.
            // (The USE_TYPO_METRICS bit is bit 7)
            if os2.fsSelection & (1 << 7) != 0 {
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

            if hhea.Ascender != 0 || hhea.Descender != 0 {
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

            let win_ascent = os2.usWinAscent as f64;
            let win_descent = os2.usWinDescent as f64;
            (
                win_ascent * px_per_unit,
                // usWinDescent is *positive* -> down unlike sTypoDescender
                // and hhea.Descender, so we flip its sign to fix this.
                -win_descent * px_per_unit,
                0.0,
            )
        };

        let (underline_position, underline_thickness): (Option<f64>, Option<f64>) =
            match post_ {
                None => (None, None),
                Some(post) => {
                    // Some fonts have degenerate 'post' tables where the underline
                    // thickness (and often position) are 0. We consider them null
                    // if this is the case and use our own fallbacks when we calculate.
                    let has_broken_underline = post.underlineThickness == 0;

                    // If the underline position isn't 0 then we do use it,
                    // even if the thickness is't properly specified.
                    let pos = if has_broken_underline && post.underlinePosition == 0 {
                        None
                    } else {
                        Some(post.underlinePosition as f64 * px_per_unit)
                    };

                    let thick = if has_broken_underline {
                        None
                    } else {
                        Some(post.underlineThickness as f64 * px_per_unit)
                    };

                    (pos, thick)
                }
            };

        // Similar logic to the underline above.
        let (strikethrough_position, strikethrough_thickness): (Option<f64>, Option<f64>) =
            match os2_ {
                None => (None, None),
                Some(os2) => {
                    let has_broken_strikethrough = os2.yStrikeoutSize == 0;

                    let pos = if has_broken_strikethrough && os2.yStrikeoutPosition == 0 {
                        None
                    } else {
                        Some(os2.yStrikeoutPosition as f64 * px_per_unit)
                    };

                    let thick = if has_broken_strikethrough {
                        None
                    } else {
                        Some(os2.yStrikeoutSize as f64 * px_per_unit)
                    };

                    (pos, thick)
                }
            };

        // Cell width is calculated by calculating the widest width of the
        // visible ASCII characters. Usually 'M' is widest but we just take
        // whatever is widest.
        //
        // ASCII height is calculated as the height of the overall bounding
        // box of the same characters.
        //
        // If we fail to load any visible ASCII we just use max_advance from
        // the metrics provided by FreeType, and set ascii_height to null as
        // it's optional.
        let (cell_width, ascii_height): (f64, Option<f64>) = {
            let _guard = lock(&self.ft_mutex);

            let mut max: f64 = 0.0;
            let mut top: f64 = 0.0;
            let mut bottom: f64 = 0.0;
            for c in b' '..127u8 {
                if let Some(glyph_index) = face.get_char_index(c as u32) {
                    if face.load_glyph(glyph_index, self.glyph_load_flags(false)).is_ok() {
                        // SAFETY: the glyph slot holds the loaded glyph and
                        // is guarded by `ft_mutex`.
                        unsafe {
                            let glyph = (*face.handle).glyph;
                            max = f26dot6_to_f64((*glyph).advance.x).max(max);
                            let rect = Self::get_glyph_size(glyph);
                            top = (rect.y + rect.height).max(top);
                            bottom = rect.y.min(bottom);
                        }
                    }
                }
            }

            // If we couldn't get valid measurements, just use
            // FreeType's max_advance and null, respectively.
            if max == 0.0 {
                max = f26dot6_to_f64(size_metrics.max_advance);
            }
            let rect_height: Option<f64> = {
                let estimate = top - bottom;
                if estimate <= 0.0 {
                    None
                } else {
                    Some(estimate)
                }
            };

            (max, rect_height)
        };

        // We use the cap and ex heights specified by the font if they're
        // available, otherwise we try to measure the `H` and `x` glyphs.
        let (cap_height, ex_height): (Option<f64>, Option<f64>) = 'heights: {
            if let Some(os2) = os2_ {
                // The OS/2 table does not include these metrics in version 1.
                if os2.version >= 2 {
                    break 'heights (
                        Some(os2.sCapHeight as f64 * px_per_unit),
                        Some(os2.sxHeight as f64 * px_per_unit),
                    );
                }
            }

            let measure = |cp: char| -> Option<f64> {
                let _guard = lock(&self.ft_mutex);
                let glyph_index = face.get_char_index(cp as u32)?;
                face.load_glyph(glyph_index, self.glyph_load_flags(false)).ok()?;
                // SAFETY: the glyph slot holds the loaded glyph and is
                // guarded by `ft_mutex`.
                Some(unsafe { Self::get_glyph_size((*face.handle).glyph) }.height)
            };

            (measure('H'), measure('x'))
        };

        // Measure "水" (CJK water ideograph, U+6C34) for our ic width.
        let ic_width: Option<f64> = 'ic_width: {
            let _guard = lock(&self.ft_mutex);

            let Some(glyph) = face.get_char_index('水' as u32) else {
                break 'ic_width None;
            };

            if face.load_glyph(glyph, self.glyph_load_flags(false)).is_err() {
                break 'ic_width None;
            }

            // SAFETY: the glyph slot holds the loaded glyph and is guarded
            // by `ft_mutex`.
            let (ft_glyph_width, advance) = unsafe {
                let ft_glyph = (*face.handle).glyph;
                (
                    Self::get_glyph_size(ft_glyph).width,
                    f26dot6_to_f64((*ft_glyph).advance.x),
                )
            };

            // If the advance of the glyph is less than the width of the actual
            // glyph then we just treat it as invalid since it's probably wrong
            // and using it for size normalization will instead make the font
            // way too big.
            //
            // This can sometimes happen if there's a CJK font that has been
            // patched with the nerd fonts patcher and it butchers the advance
            // values so the advance ends up half the width of the actual glyph.
            if ft_glyph_width > advance {
                let mut buf = [0u8; 1024];
                let font_name: &[u8] = self.name(&mut buf).unwrap_or(b"<Error getting font name>");
                log::warn!(
                    target: LOG,
                    "(getMetrics) Width of glyph '水' for font \"{}\" is greater than its advance ({} > {}), discarding ic_width metric.",
                    String::from_utf8_lossy(font_name),
                    ft_glyph_width,
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

            cap_height,
            ex_height,
            ascii_height,
            ic_width,
        }
    }

    /// Copy the font table data for the given tag.
    pub fn copy_table(&self, tag: &[u8; 4]) -> Result<Option<Vec<u8>>, freetype::Error> {
        self.face.load_sfnt_table(freetype::Tag::init(tag))
    }
}

impl Drop for Face {
    /// Zig `deinit`.
    fn drop(&mut self) {
        {
            let _guard = lock(&self.lib.mutex);

            // SAFETY: this face owns one reference to the FreeType face.
            unsafe { self.face.deinit() };
        }
        // SAFETY: this face owns one reference to the HarfBuzz font.
        unsafe { self.hb_font.destroy() };
    }
}

/// Convert 26.6 pixel format to f32
#[allow(dead_code)]
fn f26dot6_to_float(v: c::FT_F26Dot6) -> f32 {
    (v >> 6) as f32
}

fn f26dot6_to_f64(v: c::FT_F26Dot6) -> f64 {
    F26Dot6::from_bits(i32::try_from(v).expect("26.6 value out of range")).to_f64()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::font::embedded;
    use crate::font::metrics::Metrics;

    fn opts(points: f32, dpi: u16) -> Options {
        Options::new(DesiredSize {
            points,
            xdpi: dpi,
            ydpi: dpi,
        })
    }

    #[test]
    fn face_name() {
        let mut lib = Library::init().unwrap();

        {
            let face = Face::init(lib.clone(), embedded::VARIABLE, Options::new(DesiredSize::new(14.0)))
                .unwrap();

            let mut buf = [0u8; 1024];
            let actual = face.name(&mut buf).unwrap();

            assert_eq!(b"JetBrains Mono", actual);
        }

        {
            let face =
                Face::init(lib.clone(), embedded::INCONSOLATA, Options::new(DesiredSize::new(14.0)))
                    .unwrap();

            let mut buf = [0u8; 1024];
            let actual = face.name(&mut buf).unwrap();

            assert_eq!(b"Inconsolata", actual);
        }

        lib.deinit();
    }

    #[test]
    fn render_ascii_and_resize() {
        // Zig: unnamed `test` block.
        let test_font = embedded::INCONSOLATA;

        let mut lib = Library::init().unwrap();
        {
            let mut atlas = Atlas::init(512, atlas::Format::Grayscale).unwrap();

            let mut ft_font = Face::init(lib.clone(), test_font, opts(12.0, 96)).unwrap();

            // Generate all visible ASCII
            for i in 32u8..127 {
                ft_font
                    .render_glyph(
                        &mut atlas,
                        ft_font.glyph_index(i as u32).unwrap(),
                        RenderOptions::new(Metrics::calc(ft_font.get_metrics())),
                    )
                    .unwrap();
            }

            // Test resizing
            {
                let g1 = ft_font
                    .render_glyph(
                        &mut atlas,
                        ft_font.glyph_index('A' as u32).unwrap(),
                        RenderOptions::new(Metrics::calc(ft_font.get_metrics())),
                    )
                    .unwrap();
                assert_eq!(11u32, g1.height);

                ft_font.set_size(opts(24.0, 96)).unwrap();
                let g2 = ft_font
                    .render_glyph(
                        &mut atlas,
                        ft_font.glyph_index('A' as u32).unwrap(),
                        RenderOptions::new(Metrics::calc(ft_font.get_metrics())),
                    )
                    .unwrap();
                assert_eq!(21u32, g2.height);
            }
        }
        lib.deinit();
    }

    #[test]
    fn color_emoji() {
        let test_font = embedded::EMOJI;

        let mut lib = Library::init().unwrap();
        {
            let mut atlas = Atlas::init(512, atlas::Format::Bgra).unwrap();

            let ft_font = Face::init(lib.clone(), test_font, opts(12.0, 96)).unwrap();

            ft_font
                .render_glyph(
                    &mut atlas,
                    ft_font.glyph_index('🥸' as u32).unwrap(),
                    RenderOptions::new(Metrics::calc(ft_font.get_metrics())),
                )
                .unwrap();

            // Make sure this glyph has color
            {
                assert!(ft_font.has_color());
                let glyph_id = ft_font.glyph_index('🥸' as u32).unwrap();
                assert!(ft_font.is_color_glyph(glyph_id));
            }
        }
        lib.deinit();
    }

    #[test]
    fn mono_to_bgra() {
        let test_font = embedded::EMOJI;

        let mut lib = Library::init().unwrap();
        {
            let mut atlas = Atlas::init(512, atlas::Format::Bgra).unwrap();

            let ft_font = Face::init(lib.clone(), test_font, opts(12.0, 72)).unwrap();

            // glyph 3 is mono in Noto
            ft_font
                .render_glyph(
                    &mut atlas,
                    3,
                    RenderOptions::new(Metrics::calc(ft_font.get_metrics())),
                )
                .unwrap();
        }
        lib.deinit();
    }

    #[test]
    fn svg_font_table() {
        let test_font = embedded::JULIA_MONO;

        let mut lib = Library::init().unwrap();
        {
            let face = Face::init(lib.clone(), test_font, opts(12.0, 72)).unwrap();

            let table = face.copy_table(b"SVG ").unwrap().unwrap();

            assert_eq!(430, table.len());
        }
        lib.deinit();
    }

    const TERMINUS_I: &str = "\
........
........
...#....
...#....
........
..##....
...#....
...#....
...#....
...#....
...#....
..###...
........
........
........
........";
    // Including the newline
    const TERMINUS_I_PITCH: usize = 9;

    /// Check that `glyph` in `atlas` matches the `#`/`.` pattern.
    fn expect_pattern(atlas: &Atlas, glyph: Glyph, pattern: &str, pitch: usize) {
        let pattern = pattern.as_bytes();
        for y in 0..glyph.height as usize {
            for x in 0..glyph.width as usize {
                let pixel = pattern[y * pitch + x];
                assert_eq!(
                    if pixel == b'#' { 255u8 } else { 0 },
                    atlas.data[(glyph.atlas_y as usize + y) * atlas.size as usize
                        + (glyph.atlas_x as usize + x)],
                    "pixel x={x} y={y}"
                );
            }
        }
    }

    #[test]
    fn bitmap_glyph() {
        let test_font = embedded::TERMINUS_TTF;

        let mut lib = Library::init().unwrap();
        {
            let mut atlas = Atlas::init(512, atlas::Format::Grayscale).unwrap();

            // Any glyph at 12pt @ 96 DPI is a bitmap
            let ft_font = Face::init(lib.clone(), test_font, opts(12.0, 96)).unwrap();

            // glyph 77 = 'i'
            let glyph = ft_font
                .render_glyph(
                    &mut atlas,
                    77,
                    RenderOptions::new(Metrics::calc(ft_font.get_metrics())),
                )
                .unwrap();

            // should render crisp
            assert_eq!(8, glyph.width);
            assert_eq!(16, glyph.height);
            expect_pattern(&atlas, glyph, TERMINUS_I, TERMINUS_I_PITCH);
        }
        lib.deinit();
    }

    // Expected pixel pattern for Spleen 8x16 'A' (glyph index from char 'A')
    // Derived from BDF BITMAP data: 00,00,7C,C6,C6,C6,FE,C6,C6,C6,C6,C6,00,00,00,00
    const SPLEEN_A: &str = "\
........
........
.#####..
##...##.
##...##.
##...##.
#######.
##...##.
##...##.
##...##.
##...##.
##...##.
........
........
........
........";
    // Including the newline
    const SPLEEN_A_PITCH: usize = 9;
    // Test parameters for bitmap font tests
    const SPLEEN_TEST_POINT_SIZE: f32 = 12.0;
    const SPLEEN_TEST_DPI: u16 = 96;

    fn test_spleen(test_font: &'static [u8]) {
        let mut lib = Library::init().unwrap();
        {
            let mut atlas = Atlas::init(512, atlas::Format::Grayscale).unwrap();

            // Spleen 8x16 is a pure bitmap font at 16px height
            let ft_font =
                Face::init(lib.clone(), test_font, opts(SPLEEN_TEST_POINT_SIZE, SPLEEN_TEST_DPI))
                    .unwrap();

            // Get glyph index for 'A'
            let glyph_index = ft_font.glyph_index('A' as u32).expect("GlyphNotFound");

            let glyph = ft_font
                .render_glyph(
                    &mut atlas,
                    glyph_index,
                    RenderOptions::new(Metrics::calc(ft_font.get_metrics())),
                )
                .unwrap();

            // Verify dimensions match Spleen 8x16
            assert_eq!(8, glyph.width);
            assert_eq!(16, glyph.height);

            // Verify pixel-perfect rendering
            expect_pattern(&atlas, glyph, SPLEEN_A, SPLEEN_A_PITCH);
        }
        lib.deinit();
    }

    #[test]
    fn bitmap_glyph_bdf() {
        test_spleen(embedded::SPLEEN_BDF);
    }

    #[test]
    fn bitmap_glyph_pcf() {
        test_spleen(embedded::SPLEEN_PCF);
    }

    #[test]
    fn bitmap_glyph_otb() {
        test_spleen(embedded::SPLEEN_OTB);
    }
}
