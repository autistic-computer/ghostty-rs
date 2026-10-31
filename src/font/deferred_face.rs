//! Port of `src/font/DeferredFace.zig`.
//!
//! STATUS: ported
//!
//! A deferred face represents a single font face with all the information
//! necessary to load it, but defers loading the full face until it is
//! needed.
//!
//! This allows us to have many fallback fonts to look for glyphs, but
//! only load them if they're really needed.
//!
//! Port note: Zig selects the backend-specific fields at comptime (they
//! are `void` for the other backends); here each field only exists on the
//! targets whose backend uses it (fontconfig on Linux and other Unix,
//! CoreText on macOS, the FreeType directory scan on Windows, the canvas
//! on wasm). `deinit` is `Drop`.

#[allow(unused_imports)]
use super::face::{self, Face, Variation};
#[allow(unused_imports)]
use super::library::Library;
#[allow(unused_imports)]
use super::Presentation;

#[allow(dead_code)]
const LOG: &str = "deferred_face";

/// The error type of the backend's `Face` initialization.
#[cfg(not(any(target_os = "macos", target_os = "ios", target_arch = "wasm32")))]
pub type FaceError = face::freetype::Error;

/// The error type of the backend's `Face` initialization.
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub type FaceError = face::coretext::Error;

/// The error type of the backend's `Face` initialization.
#[cfg(target_arch = "wasm32")]
pub type FaceError = face::web_canvas::Error;

/// Errors from [`DeferredFace`] operations (the Zig inferred error sets).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    OutOfMemory,
    /// A fontconfig pattern lookup failed.
    #[cfg(not(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "windows",
        target_arch = "wasm32"
    )))]
    Fontconfig(crate::pkg::fontconfig::Error),
    /// Loading the face failed.
    Face(FaceError),
    FontHasNoFile,
    FontPathCantDecode,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for Error {}

impl From<FaceError> for Error {
    fn from(e: FaceError) -> Error {
        Error::Face(e)
    }
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "windows",
    target_arch = "wasm32"
)))]
impl From<crate::pkg::fontconfig::Error> for Error {
    fn from(e: crate::pkg::fontconfig::Error) -> Error {
        Error::Fontconfig(e)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "ios", target_arch = "wasm32")))]
impl From<crate::pkg::freetype::Error> for Error {
    fn from(e: crate::pkg::freetype::Error) -> Error {
        Error::Face(e.into())
    }
}

#[derive(Debug, Default)]
pub struct DeferredFace {
    /// Fontconfig
    #[cfg(not(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "windows",
        target_arch = "wasm32"
    )))]
    pub fc: Option<Fontconfig>,

    /// CoreText
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub ct: Option<CoreText>,

    /// Windows (FreeType directory scan)
    #[cfg(target_os = "windows")]
    pub win: Option<Windows>,

    /// Canvas
    #[cfg(target_arch = "wasm32")]
    pub wc: Option<WebCanvas>,
}

/// Fontconfig specific data. This is only present if building with fontconfig.
#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "windows",
    target_arch = "wasm32"
)))]
#[derive(Debug)]
pub struct Fontconfig {
    /// The pattern for this font. This must be the "render prepared" pattern.
    /// (i.e. call FcFontRenderPrepare).
    pub pattern: crate::pkg::fontconfig::Pattern,

    /// Charset and Langset are used for quick lookup if a codepoint and
    /// presentation style are supported. They can be derived from pattern
    /// but are cached since they're frequently used.
    pub charset: crate::pkg::fontconfig::CharSet,

    /// Fontconfig views strings and langsets as interchangeable and
    /// may return either (really bad design if you ask me) as its public API.
    /// Therefore we can't assume this is always a `*const fontconfig.LangSet`.
    ///
    /// The value points into `pattern` (hence `'static`, valid for as long
    /// as this struct).
    pub langset: crate::pkg::fontconfig::Value<'static>,

    /// Variations to apply to this font.
    pub variations: Vec<Variation>,
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "windows",
    target_arch = "wasm32"
)))]
impl Drop for Fontconfig {
    /// Zig `deinit`.
    fn drop(&mut self) {
        // SAFETY: we own the pattern; the charset and langset point into it
        // and are not used afterwards.
        unsafe { self.pattern.destroy() };
    }
}

// SAFETY: the pattern (and the charset/langset pointing into it) is owned
// exclusively by this struct and only read.
#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "windows",
    target_arch = "wasm32"
)))]
unsafe impl Send for Fontconfig {}
#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "windows",
    target_arch = "wasm32"
)))]
unsafe impl Sync for Fontconfig {}

/// Windows specific data. Only present with the freetype_windows backend.
///
/// Unlike Fontconfig/CoreText which carry lightweight descriptor handles,
/// the Windows backend has no external descriptor service — the "deferred"
/// metadata is the FreeType face itself. We keep a pre-loaded face (loaded
/// at discovery time) to answer `hasCodepoint` cheaply without re-opening
/// the file on every query, and remember the path so `load()` can open a
/// fresh face at the caller's requested size/options.
#[cfg(target_os = "windows")]
#[derive(Debug)]
pub struct Windows {
    /// Path to the font file. Owned here.
    pub path: std::ffi::CString,

    /// Face index within the file (for .ttc collections).
    pub face_index: i32,

    /// Variations to apply on load.
    pub variations: Vec<Variation>,

    /// Pre-loaded face used for cheap metadata queries (glyphIndex,
    /// hasColor). The size it was opened at is irrelevant for these
    /// queries since the CMap is size-independent. Deinit'd with us.
    pub peek: Face,

    /// Whether the face presents as emoji (has color glyphs) or text.
    pub presentation: Presentation,
}

/// CoreText specific data. This is only present when building with CoreText.
#[cfg(any(target_os = "macos", target_os = "ios"))]
#[derive(Debug)]
pub struct CoreText {
    /// The initialized font
    pub font: &'static crate::pkg::macos::text::Font,

    /// Variations to apply to this font. We apply the variations to the
    /// search descriptor but sometimes when the font collection is
    /// made the variation axes are reset so we have to reapply them.
    pub variations: Vec<Variation>,
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
impl Drop for CoreText {
    /// Zig `deinit`.
    fn drop(&mut self) {
        self.font.release();
    }
}

/// WebCanvas specific data. This is only present when building with canvas.
#[cfg(target_arch = "wasm32")]
#[derive(Debug)]
pub struct WebCanvas {
    /// The string to use for the "font" attribute for the canvas
    pub font_str: String,

    /// The presentation for this font.
    pub presentation: Presentation,
}

impl DeferredFace {
    /// Returns the family name of the font.
    pub fn family_name<'a>(&'a self, buf: &'a mut [u8]) -> Result<&'a [u8], Error> {
        let _ = &buf;

        #[cfg(not(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "windows",
            target_arch = "wasm32"
        )))]
        if let Some(fc) = &self.fc {
            return fontconfig_string(fc, crate::pkg::fontconfig::Property::Family);
        }

        #[cfg(target_os = "windows")]
        if let Some(w) = &self.win {
            return w.peek.name(buf).map_err(|_| Error::OutOfMemory);
        }

        #[cfg(any(target_os = "macos", target_os = "ios"))]
        if let Some(ct) = &self.ct {
            use crate::pkg::macos;
            let Some(family_name) = ct.font.copy_attribute(macos::text::attr::FamilyName) else {
                return Ok(b"unknown");
            };
            if let Some(s) = family_name.cstring_ptr(macos::foundation::StringEncoding::Utf8) {
                return Ok(s.to_bytes());
            }
            return family_name
                .cstring(buf, macos::foundation::StringEncoding::Utf8)
                .ok_or(Error::OutOfMemory);
        }

        #[cfg(target_arch = "wasm32")]
        if let Some(wc) = &self.wc {
            return Ok(wc.font_str.as_bytes());
        }

        Ok(b"")
    }

    /// Returns the name of this face. The memory is always owned by the
    /// face so it doesn't have to be freed.
    pub fn name<'a>(&'a self, buf: &'a mut [u8]) -> Result<&'a [u8], Error> {
        let _ = &buf;

        #[cfg(not(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "windows",
            target_arch = "wasm32"
        )))]
        if let Some(fc) = &self.fc {
            return fontconfig_string(fc, crate::pkg::fontconfig::Property::Fullname);
        }

        #[cfg(target_os = "windows")]
        if let Some(w) = &self.win {
            return w.peek.name(buf).map_err(|_| Error::OutOfMemory);
        }

        #[cfg(any(target_os = "macos", target_os = "ios"))]
        if let Some(ct) = &self.ct {
            use crate::pkg::macos;
            let Some(display_name) = ct.font.copy_display_name() else {
                return self.family_name(buf);
            };
            if let Some(s) = display_name.cstring_ptr(macos::foundation::StringEncoding::Utf8) {
                return Ok(s.to_bytes());
            }
            // "NULL if the internal storage of theString does not allow
            // this to be returned efficiently." In this case, we need
            // to allocate. But we can't return an allocated string because
            // we don't have an allocator. Let's use the stack and log it.
            return display_name
                .cstring(buf, macos::foundation::StringEncoding::Utf8)
                .ok_or(Error::OutOfMemory);
        }

        #[cfg(target_arch = "wasm32")]
        if let Some(wc) = &self.wc {
            return Ok(wc.font_str.as_bytes());
        }

        Ok(b"")
    }

    /// Load the deferred font face. This does nothing if the face is loaded.
    pub fn load(&mut self, lib: Library, opts: face::Options) -> Result<Face, Error> {
        #[cfg(not(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "windows",
            target_arch = "wasm32"
        )))]
        return self.load_fontconfig(lib, opts);

        #[cfg(target_os = "windows")]
        return self.load_windows(lib, opts);

        #[cfg(any(target_os = "macos", target_os = "ios"))]
        return self.load_core_text(lib, opts);

        #[cfg(target_arch = "wasm32")]
        return {
            let _ = lib;
            self.load_web_canvas(opts)
        };
    }

    #[cfg(not(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "windows",
        target_arch = "wasm32"
    )))]
    fn load_fontconfig(&mut self, lib: Library, opts: face::Options) -> Result<Face, Error> {
        use crate::pkg::fontconfig::{Error as FcError, Property, Value};

        let fc = self.fc.as_ref().unwrap();

        // Filename and index for our face so we can load it
        let filename = match fc.pattern.get(Property::File, 0)? {
            Value::String(s) => s,
            _ => return Err(FcError::FontconfigTypeMismatch.into()),
        };
        let face_index = match fc.pattern.get(Property::Index, 0)? {
            Value::Integer(i) => i,
            _ => return Err(FcError::FontconfigTypeMismatch.into()),
        };

        let mut face = Face::init_file(lib, filename, face_index, opts)?;
        face.set_variations(&fc.variations, opts)?;
        Ok(face)
    }

    #[cfg(target_os = "windows")]
    fn load_windows(&mut self, lib: Library, opts: face::Options) -> Result<Face, Error> {
        let w = self.win.as_ref().unwrap();

        let mut face = Face::init_file(lib, &w.path, w.face_index, opts)?;
        face.set_variations(&w.variations, opts)?;
        Ok(face)
    }

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    fn load_core_text(&mut self, lib: Library, opts: face::Options) -> Result<Face, Error> {
        let _ = lib;
        let ct = self.ct.as_ref().unwrap();
        let mut face = Face::init_font_copy(ct.font, opts)?;
        face.set_variations(&ct.variations, opts)?;
        Ok(face)
    }

    /// The `coretext_freetype` backend (a self-compiled macOS
    /// configuration; never the default backend of this port).
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    #[allow(dead_code)]
    fn load_core_text_freetype(
        &mut self,
        lib: super::library::FreetypeLibrary,
        opts: face::Options,
    ) -> Result<face::freetype::Face, Error> {
        use crate::pkg::macos;

        let ct = self.ct.as_ref().unwrap();

        // Get the URL for the font so we can get the filepath
        let url = ct.font.copy_attribute(macos::text::attr::Url).ok_or(Error::FontHasNoFile)?;

        // Get the path from the URL
        let path = url.copy_path();
        url.release();
        let path = path.ok_or(Error::FontHasNoFile)?;

        // URL decode the path
        let blank = macos::foundation::String::create_with_bytes(
            b"",
            macos::foundation::StringEncoding::Utf8,
            false,
        )
        .map_err(|_| Error::OutOfMemory)?;
        let decoded =
            macos::foundation::URL::create_string_by_replacing_percent_escapes(path, blank);
        blank.release();
        path.release();
        let decoded = decoded.map_err(|_| Error::OutOfMemory)?;

        // Decode into a c string. 1024 bytes should be enough for anybody.
        let mut buf = [0u8; 1024];
        let path_slice = decoded
            .cstring(&mut buf[..1023], macos::foundation::StringEncoding::Utf8)
            .map(|s| s.to_vec());
        decoded.release();
        let path_slice = path_slice.ok_or(Error::FontPathCantDecode)?;

        // Freetype requires null-terminated. We always leave space at
        // the end for a zero so we set that up here.
        let path_c = std::ffi::CString::new(path_slice).map_err(|_| Error::FontPathCantDecode)?;

        // Face index 0 is not always correct. We don't ship this configuration
        // in a release build. Users should use the pure CoreText builds.
        //std.log.warn("path={s}", .{path_slice});
        let mut face = face::freetype::Face::init_file(lib, &path_c, 0, opts)
            .map_err(|_| Error::FontHasNoFile)?;
        face.set_variations(&ct.variations, opts)
            .map_err(|_| Error::FontHasNoFile)?;

        Ok(face)
    }

    #[cfg(target_arch = "wasm32")]
    fn load_web_canvas(&mut self, opts: face::Options) -> Result<Face, Error> {
        let wc = self.wc.as_ref().unwrap();
        Ok(Face::init_named(&wc.font_str, opts.size, wc.presentation)?)
    }

    /// Returns true if this face can satisfy the given codepoint and
    /// presentation. If presentation is null, then it just checks if the
    /// codepoint is present at all.
    ///
    /// This should not require the face to be loaded IF we're using a
    /// discovery mechanism (i.e. fontconfig). If no discovery is used,
    /// the face is always expected to be loaded.
    pub fn has_codepoint(&self, cp: u32, p: Option<Presentation>) -> bool {
        let _ = (cp, p);

        #[cfg(not(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "windows",
            target_arch = "wasm32"
        )))]
        {
            use crate::pkg::fontconfig::Value;

            // If we are using fontconfig, use the fontconfig metadata to
            // avoid loading the face.
            if let Some(fc) = &self.fc {
                // Check if char exists
                if !fc.charset.has_char(cp) {
                    return false;
                }

                // If we have a presentation, check it matches
                if let Some(desired) = p {
                    const EMOJI_LANG: &std::ffi::CStr = c"und-zsye";
                    let is_emoji = match fc.langset {
                        Value::LangSet(ls) => ls.has_lang(EMOJI_LANG),
                        Value::String(s) => s == EMOJI_LANG,
                        v => {
                            log::warn!(
                                target: LOG,
                                "fontconfig's pattern lang is neither a lang set or a string={:?}",
                                v
                            );
                            false
                        }
                    };
                    let actual = if is_emoji {
                        Presentation::Emoji
                    } else {
                        Presentation::Text
                    };

                    return desired == actual;
                }

                return true;
            }
        }

        #[cfg(target_os = "windows")]
        {
            // Use the pre-loaded peek face for a cheap CMap lookup.
            if let Some(w) = &self.win {
                if let Some(desired) = p {
                    if w.presentation != desired {
                        return false;
                    }
                }
                return w.peek.glyph_index(cp).is_some();
            }
        }

        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            use crate::pkg::macos;

            // If we are using coretext, we check the loaded CT font.
            if let Some(ct) = &self.ct {
                // This presentation check isn't as detailed as isColorGlyph
                // because forced presentation modes are only used for emoji and
                // emoji should always have color glyphs set. This can be
                // more correct by using the isColorGlyph logic but I'd want
                // to find a font that actually requires this so we can write
                // a test for it before changing it.
                if let Some(desired_p) = p {
                    let traits = ct.font.get_symbolic_traits();
                    let actual_p = if traits.color_glyphs {
                        Presentation::Emoji
                    } else {
                        Presentation::Text
                    };
                    if actual_p != desired_p {
                        return false;
                    }
                }

                // Turn UTF-32 into UTF-16 for CT API
                let mut unichars = [0u16; 2];
                let pair =
                    macos::foundation::string_get_surrogate_pair_for_long_character(cp, &mut unichars);
                let len: usize = if pair { 2 } else { 1 };

                // Get our glyphs
                let mut glyphs: [macos::graphics::Glyph; 2] = [0, 0];
                return ct
                    .font
                    .get_glyphs_for_characters(&unichars[..len], &mut glyphs[..len]);
            }
        }

        // Canvas always has the codepoint because we have no way of
        // really checking and we let the browser handle it.
        #[cfg(target_arch = "wasm32")]
        if let Some(wc) = &self.wc {
            // Fast-path if we have a specific presentation and we
            // don't match, then it is definitely not this face.
            if let Some(desired) = p {
                if wc.presentation != desired {
                    return false;
                }
            }

            // Slow-path: we initialize the font, render it, and check
            // if it works and the presentation matches.
            let face = match Face::init_named(
                &wc.font_str,
                face::DesiredSize::new(12.0),
                wc.presentation,
            ) {
                Ok(face) => face,
                Err(err) => {
                    log::warn!(
                        target: LOG,
                        "failed to init face for codepoint check face={} err={:?}",
                        wc.font_str,
                        err,
                    );
                    return false;
                }
            };
            return face.glyph_index(cp).is_some();
        }

        // This is unreachable because discovery mechanisms terminate, and
        // if we're not using a discovery mechanism, the face MUST be loaded.
        unreachable!()
    }
}

/// Read a string property of a fontconfig pattern.
#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "windows",
    target_arch = "wasm32"
)))]
fn fontconfig_string(
    fc: &Fontconfig,
    prop: crate::pkg::fontconfig::Property,
) -> Result<&[u8], Error> {
    match fc.pattern.get(prop, 0)? {
        crate::pkg::fontconfig::Value::String(s) => {
            // SAFETY: the string is owned by the pattern, which lives as
            // long as `fc` (the `Pattern` handle is a copy, so `get` ties
            // the value to the local handle, not to `fc`).
            Ok(unsafe { std::mem::transmute::<&[u8], &[u8]>(s.to_bytes()) })
        }
        _ => Err(crate::pkg::fontconfig::Error::FontconfigTypeMismatch.into()),
    }
}

/// The wasm-compatible API.
#[cfg(target_arch = "wasm32")]
pub mod wasm {
    use super::*;

    #[no_mangle]
    pub extern "C" fn deferred_face_new(
        ptr: *const u8,
        len: usize,
        presentation: u16,
    ) -> *mut DeferredFace {
        match deferred_face_new_(ptr, len, presentation) {
            Ok(v) => v,
            Err(err) => {
                log::warn!(target: LOG, "error creating deferred face err={:?}", err);
                std::ptr::null_mut()
            }
        }
    }

    fn deferred_face_new_(
        ptr: *const u8,
        len: usize,
        presentation: u16,
    ) -> Result<*mut DeferredFace, std::str::Utf8Error> {
        // SAFETY: the host passes a valid buffer.
        let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
        let font_str = std::str::from_utf8(bytes)?.to_owned();

        let face = DeferredFace {
            wc: Some(WebCanvas {
                font_str,
                presentation: if presentation == 0 {
                    Presentation::Text
                } else {
                    Presentation::Emoji
                },
            }),
        };

        Ok(Box::into_raw(Box::new(face)))
    }

    #[no_mangle]
    pub extern "C" fn deferred_face_free(ptr: *mut DeferredFace) {
        if !ptr.is_null() {
            // SAFETY: created by `deferred_face_new`.
            drop(unsafe { Box::from_raw(ptr) });
        }
    }

    #[no_mangle]
    pub extern "C" fn deferred_face_load(this: *mut DeferredFace, pts: f32) {
        // SAFETY: created by `deferred_face_new`.
        let this = unsafe { &mut *this };
        if let Err(err) = this.load(
            Library::default(),
            face::Options::new(face::DesiredSize::new(pts)),
        ) {
            log::warn!(target: LOG, "error loading deferred face err={:?}", err);
        }
    }
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    use super::*;

    #[cfg(not(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "windows",
        target_arch = "wasm32"
    )))]
    #[test]
    fn fontconfig() {
        use crate::font::discovery;

        // Load freetype
        let mut lib = Library::init().unwrap();

        {
            // Get a deferred face from fontconfig
            let mut def = {
                let fc = discovery::Fontconfig::init(&lib);
                let mut it = fc
                    .discover(&discovery::Descriptor {
                        family: Some("monospace".into()),
                        size: 12.0,
                        ..Default::default()
                    })
                    .unwrap();
                it.next().unwrap().unwrap()
            };

            // Verify we can get the name
            let mut buf = [0u8; 1024];
            let n = def.name(&mut buf).unwrap();
            assert!(!n.is_empty());

            // Load it and verify it works
            let face = def
                .load(lib.clone(), face::Options::new(face::DesiredSize::new(12.0)))
                .unwrap();
            assert!(face.glyph_index(' ' as u32).is_some());
        }

        lib.deinit();
    }

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    #[test]
    fn coretext() {
        use crate::font::discovery;

        // Load freetype
        let mut lib = Library::init().unwrap();

        // Get a deferred face from fontconfig
        let mut def = {
            let fc = discovery::CoreText::init(&lib);
            let mut it = fc
                .discover(&discovery::Descriptor {
                    family: Some("Monaco".into()),
                    size: 12.0,
                    ..Default::default()
                })
                .unwrap();
            it.next().unwrap().unwrap()
        };
        assert!(def.has_codepoint(' ' as u32, None));

        // Verify we can get the name
        let mut buf = [0u8; 1024];
        let n = def.name(&mut buf).unwrap();
        assert!(!n.is_empty());

        // Load it and verify it works
        let face = def
            .load(lib.clone(), face::Options::new(face::DesiredSize::new(12.0)))
            .unwrap();
        assert!(face.glyph_index(' ' as u32).is_some());
        drop(face);
        lib.deinit();
    }
}
