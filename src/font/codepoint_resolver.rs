//! Port of `src/font/CodepointResolver.zig`.
//!
//! STATUS: ported
//!
//! CodepointResolver maps a codepoint to a font. It is more dynamic
//! than "Collection" since it supports mapping codepoint ranges to
//! specific fonts, searching for fallback fonts, and more.
//!
//! To initialize the codepoint resolver, manually initialize using
//! struct initialization syntax (see [`CodepointResolver::new`]). Set the
//! fields you want set, and begin using the resolver.
//!
//! Port note: `discover` is an `Arc` (the Zig field is a borrowed pointer
//! into the owning `SharedGridSet`), and the descriptor cache is keyed by
//! the descriptor hash code (the Zig map compares keys by hash only).

use std::collections::HashMap;
use std::sync::Arc;

use crate::font::atlas::{self, Atlas};
use crate::font::codepoint_map::CodepointMap;
use crate::font::collection::{self, Collection, GetFaceError, Index, PresentationMode, Special};
use crate::font::glyph::{Glyph, RenderOptions};
use crate::font::sprite;
use crate::font::{Presentation, Style};

#[allow(unused_imports)]
use crate::font::discovery::{self, Discover};

const LOG: &str = "font_codepoint_resolver";

pub struct CodepointResolver {
    /// The underlying collection of fonts. This will be modified as
    /// new fonts are found via the resolver. The resolver takes ownership
    /// of the collection and will deinit it when it is deinitialized.
    pub collection: Collection,

    /// The set of statuses and whether they're enabled or not. This defaults
    /// to true. This can be changed at runtime with no ill effect.
    pub styles: StyleStatus,

    /// If discovery is available, we'll look up fonts where we can't find
    /// the codepoint. This can be set after initialization.
    pub discover: Option<Arc<Discover>>,

    /// A map of codepoints to font requests for codepoint-level overrides.
    /// The memory associated with the map is owned by the caller and is not
    /// modified or freed by Group.
    pub codepoint_map: Option<CodepointMap>,

    /// The descriptor cache is used to cache the descriptor to font face
    /// mapping for codepoint maps.
    pub descriptor_cache: DescriptorCache,

    /// Set this to a non-null value to enable sprite glyph drawing. If this
    /// isn't enabled we'll just fall through to trying to use regular fonts
    /// to render sprite glyphs. But more than likely, if this isn't set then
    /// terminal rendering will look wrong.
    pub sprite: Option<sprite::Face>,
}

// SAFETY: the discovery handle (fontconfig/CoreText objects) is shared
// between threads exactly like the Zig code shares its `*Discover`; all
// mutation of the resolver goes through `&mut self` (the shared grid
// serializes it with its lock). The collection is Send + Sync.
unsafe impl Send for CodepointResolver {}
unsafe impl Sync for CodepointResolver {}

/// Packed array of booleans to indicate if a style is enabled or not.
/// (Zig `std.EnumArray(Style, bool)`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StyleStatus {
    pub values: [bool; 4],
}

impl StyleStatus {
    pub const fn init_fill(v: bool) -> StyleStatus {
        StyleStatus { values: [v; 4] }
    }

    pub fn get(&self, style: Style) -> bool {
        self.values[style as usize]
    }

    pub fn set(&mut self, style: Style, v: bool) {
        self.values[style as usize] = v;
    }
}

impl Default for StyleStatus {
    fn default() -> StyleStatus {
        StyleStatus::init_fill(true)
    }
}

/// Map of descriptors to faces. This is used with manual codepoint maps
/// to ensure that we don't load the same font multiple times.
///
/// Note that the current implementation will load the same font multiple
/// times if the font used for a codepoint map is identical to a font used
/// for a regular style. That's just an inefficient choice made now because
/// the implementation is simpler and codepoint maps matching a regular
/// font is a rare case.
///
/// The key is `Descriptor.hashcode()`: note that this means its possible
/// to have two different descriptors match when there is a hash collision
/// so we should button this up later.
pub type DescriptorCache = HashMap<u64, Option<Index>>;

/// The (inferred) error set of `getIndexCodepointOverride`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodepointOverrideError {
    /// Font discovery failed (the error is formatted for logging).
    Discovery(String),
    AddDeferred(collection::AddDeferredError),
}

impl std::fmt::Display for CodepointOverrideError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CodepointOverrideError::Discovery(e) => f.write_str(e),
            CodepointOverrideError::AddDeferred(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for CodepointOverrideError {}

/// The (inferred) error set of `renderGlyph`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderGlyphError {
    GetFace(GetFaceError),
    Sprite(sprite::face::RenderGlyphError),
    Face(collection::FaceError),
}

impl RenderGlyphError {
    /// True if this is `error.AtlasFull`.
    pub fn is_atlas_full(&self) -> bool {
        match self {
            RenderGlyphError::Sprite(sprite::face::RenderGlyphError::Atlas(atlas::Error::AtlasFull)) => true,
            #[cfg(not(any(target_os = "macos", target_os = "ios", target_arch = "wasm32")))]
            RenderGlyphError::Face(crate::font::face::freetype::Error::AtlasFull) => true,
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            RenderGlyphError::Face(crate::font::face::coretext::Error::AtlasFull) => true,
            _ => false,
        }
    }
}

impl std::fmt::Display for RenderGlyphError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenderGlyphError::GetFace(e) => write!(f, "{e}"),
            RenderGlyphError::Sprite(e) => write!(f, "{e}"),
            RenderGlyphError::Face(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for RenderGlyphError {}

impl From<GetFaceError> for RenderGlyphError {
    fn from(e: GetFaceError) -> RenderGlyphError {
        RenderGlyphError::GetFace(e)
    }
}

impl CodepointResolver {
    /// Zig `.{ .collection = c }`: every other field at its default.
    pub fn new(collection: Collection) -> CodepointResolver {
        CodepointResolver {
            collection,
            styles: StyleStatus::init_fill(true),
            discover: None,
            codepoint_map: None,
            descriptor_cache: DescriptorCache::new(),
            sprite: None,
        }
    }

    /// Looks up the font that should be used for a specific codepoint.
    /// The font index is valid as long as font faces aren't removed. This
    /// isn't cached; it is expected that downstream users handle caching if
    /// that is important.
    ///
    /// Optionally, a presentation format can be specified. This presentation
    /// format will be preferred but if it can't be found in this format,
    /// any format will be accepted. If presentation is null, the UCD
    /// (Unicode Character Database) will be used to determine the default
    /// presentation for the codepoint.
    /// a code point.
    ///
    /// An allocator is required because certain functionality (codepoint
    /// mapping, fallback fonts, etc.) may require memory allocation. Curiously,
    /// this function cannot error! If an error occurs for any reason, including
    /// memory allocation, the associated functionality is ignored and the
    /// resolver attempts to use a different method to satisfy the codepoint.
    /// This behavior is intentional to make the resolver apply best-effort
    /// logic to satisfy the codepoint since its better to render something
    /// than nothing.
    ///
    /// This logic is relatively complex so the exact algorithm is documented
    /// here. If this gets out of sync with the code, ask questions.
    ///
    ///   1. If a font style is requested that is disabled (i.e. bold),
    ///      we start over with the regular font style. The regular font style
    ///      cannot be disabled, but it can be replaced with a stylized font
    ///      face.
    ///
    ///   2. If there is a codepoint override for the codepoint, we satisfy
    ///      that requirement if we can, no matter what style or presentation.
    ///
    ///   3. If this is a sprite codepoint (such as an underline), then the
    ///      sprite font always is the result.
    ///
    ///   4. If the exact style and presentation request can be satisfied by
    ///      one of our loaded fonts, we return that value. We search loaded
    ///      fonts in the order they're added to the group, so the caller must
    ///      set the priority order.
    ///
    ///   5. If the style isn't regular, we restart this process at this point
    ///      but with the regular style. This lets us fall back to regular with
    ///      our loaded fonts before trying a fallback. We'd rather show a regular
    ///      version of a codepoint from a loaded font than find a new font in
    ///      the correct style because styles in other fonts often change
    ///      metrics like glyph widths.
    ///
    ///   6. If the style is regular, and font discovery is enabled, we look
    ///      for a fallback font to satisfy our request.
    ///
    ///   7. Finally, as a last resort, we fall back to restarting this whole
    ///      process with a regular font face satisfying ANY presentation for
    ///      the codepoint. If this fails, we return null.
    ///
    pub fn get_index(&mut self, cp: u32, style: Style, p: Option<Presentation>) -> Option<Index> {
        // If we've disabled a font style, then fall back to regular.
        if style != Style::Regular && !self.styles.get(style) {
            return self.get_index(cp, Style::Regular, p);
        }

        // Codepoint overrides.
        match self.get_index_codepoint_override(cp) {
            Ok(Some(idx)) => return Some(idx),
            Ok(None) => {}
            Err(err) => {
                log::warn!(target: LOG, "codepoint override failed codepoint={} err={}", cp, err);
            }
        }

        // If we have sprite drawing enabled, check if our sprite face can
        // handle this.
        if let Some(sprite) = &self.sprite {
            if sprite.has_codepoint(cp, p) {
                return Some(Index::init_special(Special::Sprite));
            }
        }

        // Build our presentation mode. If we don't have an explicit presentation
        // given then we use the UCD (Unicode Character Database) to determine
        // the default presentation. Note there is some inefficiency here because
        // we'll do this multiple times if we recurse, but this is a cached function
        // call higher up (GroupCache) so this should be rare.
        let p_mode = match p {
            Some(v) => PresentationMode::Explicit(v),
            None => PresentationMode::Default(if crate::unicode::uucode::is_emoji_presentation(cp) {
                Presentation::Emoji
            } else {
                Presentation::Text
            }),
        };

        // If we can find the exact value, then return that.
        if let Some(value) = self.collection.get_index(cp, style, p_mode) {
            return Some(value);
        }

        // If we're not a regular font style, try looking for a regular font
        // that will satisfy this request. Blindly looking for unmatched styled
        // fonts to satisfy one codepoint results in some ugly rendering.
        if style != Style::Regular {
            if let Some(value) = self.get_index(cp, Style::Regular, p) {
                return Some(value);
            }
        }

        // If we are regular, try looking for a fallback using discovery.
        #[cfg(not(target_arch = "wasm32"))]
        if style == Style::Regular {
            log::debug!(target: LOG, "searching for a fallback font for cp={:X}", cp);
            if let Some(idx) = self.discover_fallback(cp, style, p_mode) {
                return Some(idx);
            }
        }

        // If this is regular with any matching presentation, then we are done
        // there is nothing more we can do. Otherwise we fall through and do
        // an any presentation search.
        if style == Style::Regular && p_mode == PresentationMode::Any {
            return None;
        }

        // For non-regular fonts, we fall back to regular with any presentation
        self.collection.get_index(cp, Style::Regular, PresentationMode::Any)
    }

    /// The discovery part of step 6 of `getIndex` (the `discover:` block).
    #[cfg(not(target_arch = "wasm32"))]
    fn discover_fallback(&mut self, cp: u32, style: Style, p_mode: PresentationMode) -> Option<Index> {
        let disco = self.discover.clone()?;
        let load_opts = self.collection.load_options.as_ref()?;
        let desc = discovery::Descriptor {
            codepoint: cp,
            size: load_opts.size.points,
            bold: style == Style::Bold || style == Style::BoldItalic,
            italic: style == Style::Italic || style == Style::BoldItalic,
            monospace: false,
            ..Default::default()
        };
        let mut disco_it = disco.discover_fallback(&mut self.collection, &desc).ok()?;

        loop {
            let deferred_face = match disco_it.next() {
                Ok(Some(v)) => v,
                Ok(None) => break,
                Err(err) => {
                    log::warn!(target: LOG, "fallback search failed with error err={:?}", err);
                    break;
                }
            };

            // Discovery is supposed to only return faces that have our
            // codepoint but we can't search presentation in discovery so
            // we have to check it here.
            let face = collection::Entry::new(collection::AnyFace::Deferred(deferred_face), true);
            if !face.has_codepoint(cp, p_mode) {
                // Dropping the entry deinits the deferred face.
                continue;
            }
            let collection::AnyFace::Deferred(deferred_face) = face.face else {
                unreachable!()
            };

            let mut buf = [0u8; 256];
            log::info!(
                target: LOG,
                "found codepoint 0x{:X} in fallback face={}",
                cp,
                deferred_face
                    .name(&mut buf)
                    .map(|n| String::from_utf8_lossy(n).into_owned())
                    .unwrap_or_else(|_| "<error>".to_string()),
            );
            // On error the deferred face is dropped (deinit).
            return self
                .collection
                .add_deferred(
                    deferred_face,
                    collection::AddOptions {
                        style,
                        fallback: true,
                        size_adjustment: crate::font::DEFAULT_FALLBACK_ADJUSTMENT,
                    },
                )
                .ok();
        }

        log::debug!(target: LOG, "no fallback face found for cp={:X}", cp);
        None
    }

    /// Checks if the codepoint is in the map of codepoint overrides,
    /// finds the override font, and returns it.
    fn get_index_codepoint_override(&mut self, cp: u32) -> Result<Option<Index>, CodepointOverrideError> {
        // If discovery is disabled then we can't do codepoint overrides
        // since the override is based on discovery to find the font.
        #[cfg(target_arch = "wasm32")]
        {
            let _ = cp;
            Ok(None)
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            // Get our codepoint map. If we have no map set then we have no
            // codepoint overrides and we're done.
            let Some(map) = &self.codepoint_map else {
                return Ok(None);
            };

            // If we have a codepoint too large or isn't in the map, then we
            // don't have an override. The map returns a descriptor that can be
            // used with font discovery to search for a matching font.
            if cp > 0x1F_FFFF {
                return Ok(None);
            }
            let Some(desc) = map.get(cp) else {
                return Ok(None);
            };
            let desc = desc.clone();
            let desc_hash = desc.hashcode();

            // Fast path: the descriptor is already loaded. This means that we
            // already did the search before and we have an exact font for this
            // codepoint.
            let idx_: Option<Index> = match self.descriptor_cache.get(&desc_hash) {
                Some(v) => *v,
                None => 'idx: {
                    // Slow path: we have to find this descriptor and load the font
                    let Some(discover) = self.discover.clone() else {
                        return Ok(None);
                    };
                    let mut disco_it = discover
                        .discover(&desc)
                        .map_err(|e| CodepointOverrideError::Discovery(format!("{e:?}")))?;

                    let Some(face) = disco_it
                        .next()
                        .map_err(|e| CodepointOverrideError::Discovery(format!("{e:?}")))?
                    else {
                        log::warn!(
                            target: LOG,
                            "font lookup for codepoint map failed codepoint={} err=FontNotFound",
                            cp,
                        );

                        // Add null to the cache so we don't do a lookup again later.
                        self.descriptor_cache.insert(desc_hash, None);
                        return Ok(None);
                    };

                    // Add the font to our list of fonts so we can get an index for it,
                    // and ensure the index is stored in the descriptor cache for next time.
                    let idx = self
                        .collection
                        .add_deferred(
                            face,
                            collection::AddOptions {
                                style: Style::Regular,
                                fallback: false,
                                size_adjustment: crate::font::DEFAULT_FALLBACK_ADJUSTMENT,
                            },
                        )
                        .map_err(CodepointOverrideError::AddDeferred)?;
                    self.descriptor_cache.insert(desc_hash, Some(idx));

                    break 'idx Some(idx);
                }
            };

            // The descriptor cache will populate null if the descriptor is not found
            // to avoid expensive discoveries later, so if it is null then we already
            // searched and found nothing.
            let Some(idx) = idx_ else {
                return Ok(None);
            };

            // We need to verify that this index has the codepoint we want.
            if self.collection.has_codepoint(idx, cp, PresentationMode::Any) {
                log::debug!(
                    target: LOG,
                    "codepoint override based on config codepoint={} family={}",
                    cp,
                    desc.family.as_deref().unwrap_or(""),
                );

                return Ok(Some(idx));
            }

            Ok(None)
        }
    }

    /// Returns the presentation for a specific font index. This is useful for
    /// determining what atlas is needed.
    pub fn get_presentation(&mut self, index: Index, glyph_index: u32) -> Result<Presentation, GetFaceError> {
        if let Some(sp) = index.special() {
            return Ok(match sp {
                Special::Sprite => Presentation::Text,
            });
        }

        let face = self.collection.get_face(index)?;
        Ok(if face.is_color_glyph(glyph_index) {
            Presentation::Emoji
        } else {
            Presentation::Text
        })
    }

    /// Render a glyph by glyph index into the given font atlas and return
    /// metadata about it.
    ///
    /// This performs no caching, it is up to the caller to cache calls to this
    /// if they want. This will also not resize the atlas if it is full.
    ///
    /// IMPORTANT: this renders by /glyph index/ and not by /codepoint/. The caller
    /// is expected to translate codepoints to glyph indexes in some way. The most
    /// trivial way to do this is to get the Face and call glyphIndex. If you're
    /// doing text shaping, the text shaping library (i.e. HarfBuzz) will automatically
    /// determine glyph indexes for a text run.
    pub fn render_glyph(
        &mut self,
        atlas: &mut Atlas,
        index: Index,
        glyph_index: u32,
        opts: RenderOptions,
    ) -> Result<Glyph, RenderGlyphError> {
        // Special-case fonts are rendered directly.
        if let Some(sp) = index.special() {
            match sp {
                Special::Sprite => {
                    return self
                        .sprite
                        .as_ref()
                        .unwrap()
                        .render_glyph(atlas, glyph_index, opts)
                        .map_err(RenderGlyphError::Sprite);
                }
            }
        }

        let face = self.collection.get_face(index)?;
        let glyph = face
            .render_glyph(atlas, glyph_index, opts)
            .map_err(RenderGlyphError::Face)?;
        // log.warn("GLYPH={}", .{glyph});
        Ok(glyph)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::font::collection::{AddOptions, LoadOptions, SizeAdjustment};
    use crate::font::face::{DesiredSize, Face};
    use crate::font::library::Library;
    use crate::font::metrics::{FaceMetrics, Metrics};
    use crate::font::{embedded, OPTIONS};

    fn size96(points: f32) -> DesiredSize {
        DesiredSize {
            points,
            xdpi: 96,
            ydpi: 96,
        }
    }

    fn opts(style: Style) -> AddOptions {
        AddOptions {
            style,
            fallback: false,
            size_adjustment: SizeAdjustment::None,
        }
    }

    #[test]
    fn get_index() {
        let test_font = embedded::REGULAR;
        let test_emoji = embedded::EMOJI;
        let test_emoji_text = embedded::EMOJI_TEXT;

        let mut lib = Library::init().unwrap();
        {
            let mut c = Collection::init();
            c.load_options = Some(LoadOptions::new(lib.clone()));

            c.add(Face::init(lib.clone(), test_font, size96(12.0).into()).unwrap(), opts(Style::Regular))
                .unwrap();
            if !OPTIONS.backend.has_coretext() {
                // Coretext doesn't support Noto's format
                c.add(
                    Face::init(lib.clone(), test_emoji, DesiredSize::new(12.0).into()).unwrap(),
                    opts(Style::Regular),
                )
                .unwrap();
            }
            c.add(
                Face::init(lib.clone(), test_emoji_text, DesiredSize::new(12.0).into()).unwrap(),
                opts(Style::Regular),
            )
            .unwrap();

            let mut r = CodepointResolver::new(c);

            // Should find all visible ASCII
            for i in 32u32..127 {
                let idx = r.get_index(i, Style::Regular, None).unwrap();
                assert_eq!(Style::Regular, idx.style);
                assert_eq!(0, idx.idx);
            }

            // Try emoji
            {
                let idx = r.get_index('🥸' as u32, Style::Regular, None).unwrap();
                assert_eq!(Style::Regular, idx.style);
                assert_eq!(1, idx.idx);
            }

            // Try text emoji
            {
                let idx = r.get_index(0x270C, Style::Regular, Some(Presentation::Text)).unwrap();
                assert_eq!(Style::Regular, idx.style);
                let text_idx = if OPTIONS.backend.has_coretext() { 1 } else { 2 };
                assert_eq!(text_idx, idx.idx);
            }
            {
                let idx = r.get_index(0x270C, Style::Regular, Some(Presentation::Emoji)).unwrap();
                assert_eq!(Style::Regular, idx.style);
                assert_eq!(1, idx.idx);
            }

            // Box glyph should be null since we didn't set a box font
            {
                assert!(r.get_index(0x1FB00, Style::Regular, None).is_none());
            }
        }
        lib.deinit();
    }

    #[test]
    fn get_index_disabled_font_style() {
        let test_font = embedded::REGULAR;

        let _atlas_grayscale = Atlas::init(512, atlas::Format::Grayscale).unwrap();

        let mut lib = Library::init().unwrap();
        {
            let mut c = Collection::init();
            c.load_options = Some(LoadOptions::new(lib.clone()));

            c.add(Face::init(lib.clone(), test_font, size96(12.0).into()).unwrap(), opts(Style::Regular))
                .unwrap();
            c.add(Face::init(lib.clone(), test_font, size96(12.0).into()).unwrap(), opts(Style::Bold))
                .unwrap();
            c.add(Face::init(lib.clone(), test_font, size96(12.0).into()).unwrap(), opts(Style::Italic))
                .unwrap();

            let mut r = CodepointResolver::new(c);
            r.styles.set(Style::Bold, false); // Disable bold

            // Regular should work fine
            {
                let idx = r.get_index('A' as u32, Style::Regular, None).unwrap();
                assert_eq!(Style::Regular, idx.style);
                assert_eq!(0, idx.idx);
            }

            // Bold should go to regular
            {
                let idx = r.get_index('A' as u32, Style::Bold, None).unwrap();
                assert_eq!(Style::Regular, idx.style);
                assert_eq!(0, idx.idx);
            }

            // Italic should still work
            {
                let idx = r.get_index('A' as u32, Style::Italic, None).unwrap();
                assert_eq!(Style::Italic, idx.style);
                assert_eq!(0, idx.idx);
            }
        }
        lib.deinit();
    }

    #[test]
    fn get_index_box_glyph() {
        let mut lib = Library::init().unwrap();
        {
            let c = Collection::init();

            let mut r = CodepointResolver::new(c);
            r.sprite = Some(sprite::Face {
                metrics: Metrics::calc(FaceMetrics {
                    px_per_em: 30.0,
                    cell_width: 18.0,
                    ascent: 30.0,
                    descent: -6.0,
                    line_gap: 0.0,
                    ..Default::default()
                }),
            });

            // Should find a box glyph
            let idx = r.get_index(0x2500, Style::Regular, None).unwrap();
            assert_eq!(Style::Regular, idx.style);
            assert_eq!(Special::Sprite as u16, idx.idx);
        }
        lib.deinit();
    }
}
