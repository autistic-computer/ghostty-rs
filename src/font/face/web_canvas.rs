//! Port of `src/font/face/web_canvas.zig`.
//!
//! STATUS: ported (wasm32-only; not compiled on Linux. The `zig-js`
//! package it uses has no Rust port yet, so a minimal stand-in `js` module
//! with the same object model is defined below; it has never been
//! compiled.)
//!
//! Port notes: the Zig file predates several refactors (it builds a
//! `font.Metrics` with only some fields and matches on an `.rgba` atlas
//! format that no longer exists). This port keeps the logic; the missing
//! `Metrics` fields are zero and the `.rgba` case is `Bgra`.

#![cfg(target_arch = "wasm32")]

use std::collections::HashMap;

use crate::font::atlas::{self, Atlas};
use crate::font::face::DesiredSize;
use crate::font::glyph::{Glyph, RenderOptions};
use crate::font::metrics::Metrics;
use crate::font::Presentation;
use crate::quirks::inline_assert as assert;

const LOG: &str = "font_face";

/// See graphemes field for more details.
const GRAPHEME_START: u32 = 0x10FFFF + 1;
#[allow(dead_code)]
const GRAPHEME_END: u32 = u32::MAX;

/// Errors from the web canvas face (the Zig inferred error sets).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    OutOfMemory,
    AtlasFull,
    /// A JS call failed.
    Js,
    UnknownGraphemeCluster,
    UnsupportedAtlasFormat,
    InvalidCodepoint,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for Error {}

impl From<js::Error> for Error {
    fn from(_: js::Error) -> Error {
        Error::Js
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

#[derive(Debug)]
pub struct Face {
    /// The CSS "font" attribute, excluding size.
    pub font_str: String,

    /// The size we currently have set.
    pub size: DesiredSize,

    /// The presentation for this font.
    pub presentation: Presentation,

    /// Metrics for this font face. These are useful for renderers.
    pub metrics: Metrics,

    /// The canvas element that we will reuse to render glyphs
    pub canvas: js::Object,

    /// The map to store multi-codepoint grapheme clusters that are rendered.
    /// We use 1 above the maximum unicode codepoint up to the max 32-bit
    /// unsigned integer to store the "glyph index" for graphemes.
    pub grapheme_to_glyph: HashMap<Vec<u8>, u32>,
    pub glyph_to_grapheme: HashMap<u32, Vec<u8>>,
    pub grapheme_next: u32,
}

/// An internal (web-canvas-only) format for rendered glyphs
/// since we do render passes in multiple different situations.
struct RenderedGlyph {
    #[allow(dead_code)]
    metrics: js::Object,
    width: u32,
    height: u32,
    bitmap: Vec<u8>,
}

impl Face {
    /// Initialize a web canvas font with a "raw" value. The "raw" value can
    /// be any valid value for a CSS "font" property EXCLUDING the size. The
    /// size is always added via the `size` parameter.
    ///
    /// The raw value is copied so the caller can free it after it is gone.
    ///
    /// The presentation is given here directly because the browser gives
    /// us no easy way to determine the presentation we want for this font.
    /// Callers should just tell us what to expect.
    pub fn init_named(
        raw: &str,
        size: DesiredSize,
        presentation: Presentation,
    ) -> Result<Face, Error> {
        // Copy our font string because we're going to have to reuse it.
        let font_str = raw.to_owned();

        // Create our canvas that we're going to continue to reuse.
        let doc = js::global().get_object("document")?;
        let canvas = doc.call_object("createElement", &[js::Value::Str("canvas")])?;

        let mut result = Face {
            font_str,
            size,
            presentation,

            canvas,

            // We're going to calculate these right after initialization.
            metrics: zero_metrics(),

            grapheme_to_glyph: HashMap::new(),
            glyph_to_grapheme: HashMap::new(),
            grapheme_next: GRAPHEME_START,
        };
        result.calc_metrics()?;

        log::debug!(target: LOG, "face initialized: {}", raw);
        Ok(result)
    }

    /// Resize the font in-place. If this succeeds, the caller is responsible
    /// for clearing any glyph caches, font atlas data, etc.
    pub fn set_size(&mut self, size: DesiredSize) -> Result<(), Error> {
        let old = self.size;
        self.size = size;
        if let Err(err) = self.calc_metrics() {
            self.size = old;
            return Err(err);
        }
        Ok(())
    }

    /// Returns the glyph index for the given Unicode code point. For canvas,
    /// we support every glyph and the ID is just the codepoint since we don't
    /// have access to the underlying tables anyways. We let the browser deal
    /// with bad codepoints.
    pub fn glyph_index(&self, cp: u32) -> Option<u32> {
        // If this is a multi-codepoint grapheme then we only check if
        // we actually know about it.
        if cp >= GRAPHEME_START && !self.glyph_to_grapheme.contains_key(&cp) {
            return None;
        }

        // Render the glyph to determine if it is colored or not. We
        // have to do this because the browser will always try to render
        // whatever we give it and we have no API to determine color.
        //
        // We don't want to say yes to the wrong presentation because
        // it will go into the wrong Atlas.
        let p = if cp <= 255 {
            Presentation::Text
        } else {
            // In this case, we assume we are unable to render
            // this glyph and therefore just say we don't support it.
            self.glyph_presentation(cp).ok()?
        };
        if p != self.presentation {
            return None;
        }

        Some(cp)
    }

    /// This determines the presentation of the glyph by literally
    /// inspecting the image data to look for any color. This isn't
    /// super performant but we don't have a better choice given the
    /// canvas APIs.
    fn glyph_presentation(&self, cp: u32) -> Result<Presentation, Error> {
        // Render the glyph
        let render = self.render_glyph_internal(cp)?;

        // Inspect the image data for any non-zeros in the RGB value.
        // NOTE(perf): this is an easy candidate for SIMD.
        for px in render.bitmap.chunks_exact(4) {
            if px[0] > 0 || px[1] > 0 || px[2] > 0 {
                return Ok(Presentation::Emoji);
            }
        }

        Ok(Presentation::Text)
    }

    /// Returns the glyph index for the given grapheme cluster. The same
    /// cluster will always map to the same glyph index. This does not render
    /// the grapheme at this time, only reserves the index.
    pub fn grapheme_glyph_index(&mut self, cluster: &[u8]) -> Result<u32, Error> {
        // If we already have this stored then return it
        if let Some(&v) = self.grapheme_to_glyph.get(cluster) {
            return Ok(v);
        }

        // We don't have it stored. Ensure we have space to store. The
        // next will be "0" if we're out of space due to unsigned int wrapping.
        if self.grapheme_next == 0 {
            return Err(Error::OutOfMemory);
        }

        // Store it
        let v = self.grapheme_next;
        self.grapheme_to_glyph.insert(cluster.to_vec(), v);
        self.glyph_to_grapheme.insert(v, cluster.to_vec());

        self.grapheme_next = self.grapheme_next.wrapping_add(1);
        Ok(v)
    }

    /// Render a glyph using the glyph index. The rendered glyph is stored
    /// in the given texture atlas.
    pub fn render_glyph(
        &self,
        atlas: &mut Atlas,
        glyph_index: u32,
        opts: RenderOptions,
    ) -> Result<Glyph, Error> {
        let _ = opts;

        let render = self.render_glyph_internal(glyph_index)?;

        // Convert the format of the bitmap if necessary
        let bitmap_a8;
        let bitmap_formatted: &[u8] = match atlas.format {
            // Bitmap is already in RGBA
            atlas::Format::Bgra => &render.bitmap,

            // Convert down to A8
            atlas::Format::Grayscale => {
                assert(render.bitmap.len() % 4 == 0);
                bitmap_a8 = render
                    .bitmap
                    .chunks_exact(4)
                    .map(|px| px[3])
                    .collect::<Vec<u8>>();
                &bitmap_a8
            }

            _ => return Err(Error::UnsupportedAtlasFormat),
        };

        // Put it in our atlas
        let region = atlas.reserve(render.width, render.height)?;
        if region.width > 0 && region.height > 0 {
            atlas.set(region, bitmap_formatted);
        }

        Ok(Glyph {
            width: render.width,
            height: render.height,
            // TODO: this can't be right
            offset_x: 0,
            offset_y: 0,
            atlas_x: region.x,
            atlas_y: region.y,
        })
    }

    /// Calculate the metrics associated with a given face.
    fn calc_metrics(&mut self) -> Result<(), Error> {
        let ctx = self.context()?;

        // Cell width is the width of our M text
        let cell_width: f32 = {
            let metrics = ctx.call_object("measureText", &[js::Value::Str("M")])?;

            // We prefer the bounding box since it is tighter but certain
            // text such as emoji do not have a bounding box set so we use
            // the full run width instead.
            let bounding_right = metrics.get_f64("actualBoundingBoxRight")? as f32;
            if bounding_right > 0.0 {
                bounding_right
            } else {
                metrics.get_f64("width")? as f32
            }
        };

        // To get the cell height we render a high and low character and get
        // the total of the ascent and descent. This should equal our
        // pixel height but this is a more surefire way to get it.
        let height_metrics = ctx.call_object("measureText", &[js::Value::Str("M_")])?;
        let asc = height_metrics.get_f64("actualBoundingBoxAscent")? as f32;
        let desc = height_metrics.get_f64("actualBoundingBoxDescent")? as f32;
        let cell_height = asc + desc;
        let cell_baseline = desc;

        // There isn't a declared underline position for canvas measurements
        // so we just go 1 under the cell height to match freetype logic
        // at this time (our freetype logic).
        let underline_position = cell_height - 1.0;
        let underline_thickness: f32 = 1.0;

        let result = Metrics {
            cell_width: cell_width as u32,
            cell_height: cell_height as u32,
            cell_baseline: cell_baseline as u32,
            underline_position: underline_position as u32,
            underline_thickness: underline_thickness as u32,
            strikethrough_position: underline_position as u32,
            strikethrough_thickness: underline_thickness as u32,
            ..zero_metrics()
        };

        self.metrics = result;
        log::debug!(target: LOG, "metrics font={} value={:?}", self.font_str, self.metrics);
        Ok(())
    }

    /// Returns the 2d context configured for drawing
    fn context(&self) -> Result<js::Object, Error> {
        // This will return the same context on subsequent calls so it
        // is important to reset it.
        let ctx = self.canvas.call_object("getContext", &[js::Value::Str("2d")])?;

        // Clear the canvas
        {
            let width = self.canvas.get_f64("width")?;
            let height = self.canvas.get_f64("height")?;
            ctx.call_void(
                "clearRect",
                &[
                    js::Value::F64(0.0),
                    js::Value::F64(0.0),
                    js::Value::F64(width),
                    js::Value::F64(height),
                ],
            )?;
        }

        // Set our context font
        let font_val = format!("{}px {}", self.size.points, self.font_str);
        ctx.set("font", js::Value::Str(&font_val))?;

        // If the font property didn't change, then the font set didn't work.
        // We do this check because it is very easy to put an invalid font
        // in and this at least makes it show up in the logs.
        let check = ctx.get_string("font")?;
        if font_val != check {
            log::warn!(
                target: LOG,
                "canvas font didn't set, fonts may be broken, expected={} got={}",
                font_val,
                check,
            );
        }

        Ok(ctx)
    }

    /// Shared logic for rendering a glyph.
    fn render_glyph_internal(&self, glyph_index: u32) -> Result<RenderedGlyph, Error> {
        // Encode our glyph to UTF-8 so we can build a JS string out of it.
        let mut utf8 = [0u8; 4];
        let glyph_str: &str = if glyph_index < GRAPHEME_START {
            // If we are a normal glyph then we are a single codepoint and
            // we just UTF8 encode it as-is.
            let c = char::from_u32(glyph_index).ok_or(Error::InvalidCodepoint)?;
            c.encode_utf8(&mut utf8)
        } else {
            // We are a multi-codepoint glyph so we have to read the glyph
            // from the map and it is already utf8 encoded.
            let slice = self
                .glyph_to_grapheme
                .get(&glyph_index)
                .ok_or(Error::UnknownGraphemeCluster)?;
            std::str::from_utf8(slice).map_err(|_| Error::UnknownGraphemeCluster)?
        };

        // Get our drawing context
        let measure_ctx = self.context()?;

        // Get the width and height of the render
        let metrics = measure_ctx.call_object("measureText", &[js::Value::Str(glyph_str)])?;
        let width: u32 = {
            // We prefer the bounding box since it is tighter but certain
            // text such as emoji do not have a bounding box set so we use
            // the full run width instead.
            let bounding_right = metrics.get_f64("actualBoundingBoxRight")? as f32;
            let w = if bounding_right > 0.0 {
                bounding_right
            } else {
                metrics.get_f64("width")? as f32
            };
            w.ceil() as u32
        } + 1;

        let left = metrics.get_f64("actualBoundingBoxLeft")? as f32;
        let asc = metrics.get_f64("actualBoundingBoxAscent")? as f32;
        let desc = metrics.get_f64("actualBoundingBoxDescent")? as f32;

        // On Firefox on Linux, the bounding box is broken in some cases for
        // ideographic glyphs (such as emoji). We detect this and behave
        // differently.
        let broken_bbox = asc + desc < 0.001;

        // Height is our ascender + descender for this char
        let height = if !broken_bbox {
            (asc + desc).ceil() as u32 + 1
        } else {
            width
        };

        // Note: width and height both get "+ 1" added to them above. This
        // is important so that there is a 1px border around the glyph to avoid
        // any clipping in the atlas.

        // Resize canvas to match the glyph size exactly
        {
            self.canvas.set("width", js::Value::F64(width as f64))?;
            self.canvas.set("height", js::Value::F64(height as f64))?;

            let width_str = format!("{width}px");
            let height_str = format!("{height}px");

            let style = self.canvas.get_object("style")?;
            style.set("width", js::Value::Str(&width_str))?;
            style.set("height", js::Value::Str(&height_str))?;
        }

        // Reload our context since we resized the canvas
        let ctx = self.context()?;

        // For the broken bounding box case we render ideographic baselines
        // so that we just render the glyph fully in the box with no offsets.
        if broken_bbox {
            ctx.set("textBaseline", js::Value::Str("ideographic"))?;
        }

        // Draw background
        ctx.set("fillStyle", js::Value::Str("transparent"))?;
        ctx.call_void(
            "fillRect",
            &[
                js::Value::F64(0.0),
                js::Value::F64(0.0),
                js::Value::F64(width as f64),
                js::Value::F64(height as f64),
            ],
        )?;

        // Draw glyph
        ctx.set("fillStyle", js::Value::Str("black"))?;
        ctx.call_void(
            "fillText",
            &[
                js::Value::Str(glyph_str),
                js::Value::F64((left + 1.0) as f64),
                js::Value::F64(if !broken_bbox {
                    (asc + 1.0) as f64
                } else {
                    height as f64
                }),
            ],
        )?;

        // Read the image data and get it into a []u8 on our side
        let bitmap: Vec<u8> = {
            // Read the raw bitmap data and get the "data" value which is a
            // Uint8ClampedArray.
            let data = ctx.call_object(
                "getImageData",
                &[
                    js::Value::F64(0.0),
                    js::Value::F64(0.0),
                    js::Value::F64(width as f64),
                    js::Value::F64(height as f64),
                ],
            )?;
            let src_array = data.get_object("data")?;

            // Allocate our local memory to copy the data to.
            let len = src_array.get_f64("length")? as usize;
            let mut bitmap = vec![0u8; len];

            // Copy the typed array into our memory.
            src_array.copy_bytes_into(&mut bitmap)?;

            bitmap
        };

        Ok(RenderedGlyph {
            metrics,
            width,
            height,
            bitmap,
        })
    }
}

/// A `Metrics` with every field zero (the Zig code leaves the fields it
/// doesn't know undefined/missing).
fn zero_metrics() -> Metrics {
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
        cursor_thickness: 0,
        cursor_height: 0,
        icon_height: 0.0,
        icon_height_single: 0.0,
        face_width: 0.0,
        face_height: 0.0,
        face_y: 0.0,
    }
}

/// The wasm-compatible API.
pub mod wasm {
    use super::*;

    #[no_mangle]
    pub extern "C" fn face_new(ptr: *const u8, len: usize, pts: u16, p: u16) -> *mut Face {
        face_new_(ptr, len, pts as f32, p).unwrap_or(std::ptr::null_mut())
    }

    fn face_new_(
        ptr: *const u8,
        len: usize,
        pts: f32,
        presentation: u16,
    ) -> Result<*mut Face, Error> {
        // SAFETY: the host passes a valid buffer.
        let raw = unsafe { std::slice::from_raw_parts(ptr, len) };
        let raw = std::str::from_utf8(raw).map_err(|_| Error::Js)?;
        let face = Face::init_named(
            raw,
            DesiredSize::new(pts),
            if presentation == 0 {
                Presentation::Text
            } else {
                Presentation::Emoji
            },
        )?;

        Ok(Box::into_raw(Box::new(face)))
    }

    #[no_mangle]
    pub extern "C" fn face_free(ptr: *mut Face) {
        if !ptr.is_null() {
            // SAFETY: created by `face_new`.
            drop(unsafe { Box::from_raw(ptr) });
        }
    }

    /// Resulting pointer must be freed using the global "free".
    #[no_mangle]
    pub extern "C" fn face_render_glyph(
        face: *mut Face,
        atlas: *mut Atlas,
        codepoint: u32,
    ) -> *mut Glyph {
        // SAFETY: the host passes pointers created by the wasm API.
        let (face, atlas) = unsafe { (&mut *face, &mut *atlas) };
        match face_render_glyph_(face, atlas, codepoint) {
            Ok(v) => v,
            Err(err) => {
                log::warn!(target: LOG, "error rendering glyph err={:?}", err);
                std::ptr::null_mut()
            }
        }
    }

    #[no_mangle]
    pub extern "C" fn face_debug_canvas(face: *mut Face) {
        // SAFETY: created by `face_new`.
        let face = unsafe { &mut *face };
        if let Err(err) = face_debug_canvas_(face) {
            log::warn!(target: LOG, "error adding debug canvas err={:?}", err);
        }
    }

    fn face_debug_canvas_(face: &mut Face) -> Result<(), Error> {
        let doc = js::global().get_object("document")?;

        let elem = doc
            .call_optional_object("getElementById", &[js::Value::Str("face-canvas")])?
            .ok_or(Error::Js)?;

        elem.call_void("append", &[js::Value::Object(&face.canvas)])?;
        Ok(())
    }

    fn face_render_glyph_(
        face: &mut Face,
        atlas: &mut Atlas,
        codepoint: u32,
    ) -> Result<*mut Glyph, Error> {
        let metrics = face.metrics;
        let glyph = face.render_glyph(atlas, codepoint, RenderOptions::new(metrics))?;
        // NOTE(port): Zig hands the result to the host with
        // `wasm.toHostOwned`, which no longer exists in `os/wasm.zig` (this
        // Zig file predates that refactor); here it is leaked with
        // Box::into_raw.
        Ok(Box::into_raw(Box::new(glyph)))
    }
}

/// Minimal stand-in for the `zig-js` package (JS object handles imported
/// from the host).
// NOTE(port): zig-js is a third-party Zig package with no Rust port.
pub mod js {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Error;

    /// A reference to a JS object owned by the host.
    #[derive(Debug)]
    pub struct Object(u32);

    /// An argument value passed to JS.
    #[derive(Debug, Clone, Copy)]
    pub enum Value<'a> {
        F64(f64),
        Str(&'a str),
        Object(&'a Object),
    }

    #[repr(C)]
    struct RawValue {
        tag: u32,
        ptr: usize,
        len: usize,
        num: f64,
    }

    impl Value<'_> {
        fn raw(&self) -> RawValue {
            match *self {
                Value::F64(num) => RawValue {
                    tag: 0,
                    ptr: 0,
                    len: 0,
                    num,
                },
                Value::Str(s) => RawValue {
                    tag: 1,
                    ptr: s.as_ptr() as usize,
                    len: s.len(),
                    num: 0.0,
                },
                Value::Object(o) => RawValue {
                    tag: 2,
                    ptr: o.0 as usize,
                    len: 0,
                    num: 0.0,
                },
            }
        }
    }

    #[link(wasm_import_module = "ghostty_js")]
    extern "C" {
        fn js_global() -> u32;
        fn js_get_object(obj: u32, name: *const u8, len: usize) -> u32;
        fn js_get_f64(obj: u32, name: *const u8, len: usize, out: *mut f64) -> u32;
        fn js_get_string(obj: u32, name: *const u8, len: usize, buf: *mut u8, cap: usize) -> isize;
        fn js_set(obj: u32, name: *const u8, len: usize, value: *const RawValue) -> u32;
        fn js_call(
            obj: u32,
            name: *const u8,
            len: usize,
            args: *const RawValue,
            nargs: usize,
            out: *mut u32,
        ) -> u32;
        fn js_copy_bytes(obj: u32, buf: *mut u8, len: usize) -> u32;
        fn js_release(obj: u32);
    }

    /// The JS global object.
    pub fn global() -> Object {
        // SAFETY: host import.
        Object(unsafe { js_global() })
    }

    impl Object {
        pub fn get_object(&self, name: &str) -> Result<Object, Error> {
            // SAFETY: host import with valid buffers.
            let id = unsafe { js_get_object(self.0, name.as_ptr(), name.len()) };
            if id == 0 {
                Err(Error)
            } else {
                Ok(Object(id))
            }
        }

        pub fn get_f64(&self, name: &str) -> Result<f64, Error> {
            let mut out = 0.0;
            // SAFETY: host import with valid buffers.
            if unsafe { js_get_f64(self.0, name.as_ptr(), name.len(), &mut out) } != 0 {
                return Err(Error);
            }
            Ok(out)
        }

        pub fn get_string(&self, name: &str) -> Result<String, Error> {
            let mut buf = vec![0u8; 256];
            loop {
                // SAFETY: host import with valid buffers.
                let n = unsafe {
                    js_get_string(self.0, name.as_ptr(), name.len(), buf.as_mut_ptr(), buf.len())
                };
                if n < 0 {
                    return Err(Error);
                }
                let n = n as usize;
                if n <= buf.len() {
                    buf.truncate(n);
                    return String::from_utf8(buf).map_err(|_| Error);
                }
                buf.resize(n, 0);
            }
        }

        pub fn set(&self, name: &str, value: Value<'_>) -> Result<(), Error> {
            let raw = value.raw();
            // SAFETY: host import with valid buffers.
            if unsafe { js_set(self.0, name.as_ptr(), name.len(), &raw) } != 0 {
                return Err(Error);
            }
            Ok(())
        }

        fn call_raw(&self, name: &str, args: &[Value<'_>]) -> Result<u32, Error> {
            let raw: Vec<RawValue> = args.iter().map(|a| a.raw()).collect();
            let mut out = 0;
            // SAFETY: host import with valid buffers.
            if unsafe {
                js_call(self.0, name.as_ptr(), name.len(), raw.as_ptr(), raw.len(), &mut out)
            } != 0
            {
                return Err(Error);
            }
            Ok(out)
        }

        pub fn call_void(&self, name: &str, args: &[Value<'_>]) -> Result<(), Error> {
            let out = self.call_raw(name, args)?;
            if out != 0 {
                drop(Object(out));
            }
            Ok(())
        }

        pub fn call_object(&self, name: &str, args: &[Value<'_>]) -> Result<Object, Error> {
            self.call_optional_object(name, args)?.ok_or(Error)
        }

        pub fn call_optional_object(
            &self,
            name: &str,
            args: &[Value<'_>],
        ) -> Result<Option<Object>, Error> {
            let out = self.call_raw(name, args)?;
            Ok(if out == 0 { None } else { Some(Object(out)) })
        }

        /// Copy the bytes of a typed array into `buf`.
        pub fn copy_bytes_into(&self, buf: &mut [u8]) -> Result<(), Error> {
            // SAFETY: host import with valid buffers.
            if unsafe { js_copy_bytes(self.0, buf.as_mut_ptr(), buf.len()) } != 0 {
                return Err(Error);
            }
            Ok(())
        }
    }

    impl Drop for Object {
        fn drop(&mut self) {
            // SAFETY: host import.
            unsafe { js_release(self.0) }
        }
    }
}
