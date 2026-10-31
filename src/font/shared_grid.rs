//! Port of `src/font/SharedGrid.zig`.
//!
//! STATUS: ported
//!
//! This structure represents the state required to render a terminal
//! grid using the font subsystem. It is "shared" because it is able to
//! be shared across multiple surfaces.
//!
//! It is desirable for the grid state to be shared because the font
//! configuration for a set of surfaces is almost always the same and
//! font data is relatively memory intensive. Further, the font subsystem
//! should be read-heavy compared to write-heavy, so it handles concurrent
//! reads well. Going even further, the font subsystem should be very rarely
//! read at all since it should only be necessary when the grid actively
//! changes.
//!
//! SharedGrid does NOT support resizing, font-family changes, font removals
//! in collections, etc. Because the Grid is shared this would cause a
//! major disruption in the rendering of multiple surfaces (i.e. increasing
//! the font size in one would increase it in all). In many cases this isn't
//! desirable so to implement configuration changes the grid should be
//! reinitialized and all surfaces should switch over to using that one.
//!
//! Port note: the Zig struct has a `lock: std.Io.RwLock` next to the
//! fields it protects, and callers lock it manually. Here the protected
//! fields (`codepoints`, `glyphs`, `atlas_grayscale`, `atlas_color`,
//! `resolver`) live in [`State`] inside `lock: RwLock<State>`, so callers
//! that need direct access (e.g. to the atlas) use `grid.lock.read()` /
//! `grid.lock.write()`. `metrics` is immutable after `init` and lives
//! outside the lock. All methods take `&self`.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::font::atlas::{self, Atlas};
use crate::font::codepoint_resolver::{self, CodepointResolver};
use crate::font::collection::{self, GetFaceError, Index, PresentationMode};
use crate::font::glyph::{Constraint, ConstraintAlign, ConstraintSize, Glyph, RenderOptions};
use crate::font::metrics::Metrics;
use crate::font::{Presentation, Style};
use crate::renderer::size::CellSize;
use crate::tripwire;

#[allow(dead_code)]
const LOG: &str = "font_shared_grid";

/// The data protected by [`SharedGrid::lock`].
pub struct State {
    /// Cache for codepoints to font indexes in a group.
    pub codepoints: HashMap<CodepointKey, Option<Index>, KeyHashBuilder>,

    /// Cache for glyph renders into the atlas.
    pub glyphs: HashMap<GlyphKey, Render, KeyHashBuilder>,

    /// The texture atlas to store renders in. The Glyph data in the glyphs
    /// cache is dependent on the atlas matching.
    pub atlas_grayscale: Atlas,
    pub atlas_color: Atlas,

    /// The underlying resolver for font data, fallbacks, etc. The shared
    /// grid takes ownership of the resolver and will free it.
    pub resolver: CodepointResolver,
}

pub struct SharedGrid {
    /// The currently active grid metrics dictating the layout of the grid.
    /// This is calculated based on the resolver and current fonts.
    pub metrics: Metrics,

    /// The RwLock used to protect the shared grid. Callers are expected to use
    /// this directly if they need to i.e. access the atlas directly. Because
    /// callers can use this lock directly, maintainers need to be extra careful
    /// to review call sites to ensure they are using the lock correctly.
    pub lock: RwLock<State>,
}

/// Fail points of [`SharedGrid::init`] (Zig `init_tw.FailPoint`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitFailPoint {
    CodepointsCapacity,
    GlyphsCapacity,
    ReloadMetrics,
}

impl InitFailPoint {
    pub const ALL: [InitFailPoint; 3] = [
        InitFailPoint::CodepointsCapacity,
        InitFailPoint::GlyphsCapacity,
        InitFailPoint::ReloadMetrics,
    ];
}

pub static INIT_TW: tripwire::Module<InitFailPoint, InitError> = tripwire::Module::new();

/// Zig `InitError` (`Allocator.Error || Collection.UpdateMetricsError`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitError {
    OutOfMemory,
    CannotLoadPrimaryFont,
}

impl std::fmt::Display for InitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for InitError {}

impl From<atlas::AllocError> for InitError {
    fn from(_: atlas::AllocError) -> InitError {
        InitError::OutOfMemory
    }
}

impl From<collection::UpdateMetricsError> for InitError {
    fn from(e: collection::UpdateMetricsError) -> InitError {
        match e {
            collection::UpdateMetricsError::CannotLoadPrimaryFont => InitError::CannotLoadPrimaryFont,
        }
    }
}

/// Fail points of [`SharedGrid::render_glyph`] (Zig `renderGlyph_tw.FailPoint`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderGlyphFailPoint {
    GetPresentation,
}

pub static RENDER_GLYPH_TW: tripwire::Module<RenderGlyphFailPoint, RenderGlyphError> = tripwire::Module::new();

/// Zig `RenderGlyphError`. The Zig version is a massive manual error set
/// (the union of every error in the call stack); here the nested error
/// types are wrapped instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderGlyphError {
    OutOfMemory,
    /// Getting the face (to determine the presentation) failed.
    GetFace(GetFaceError),
    /// Rendering the glyph failed.
    Render(codepoint_resolver::RenderGlyphError),
}

impl std::fmt::Display for RenderGlyphError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenderGlyphError::OutOfMemory => f.write_str("OutOfMemory"),
            RenderGlyphError::GetFace(e) => write!(f, "{e}"),
            RenderGlyphError::Render(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for RenderGlyphError {}

impl From<GetFaceError> for RenderGlyphError {
    fn from(e: GetFaceError) -> RenderGlyphError {
        RenderGlyphError::GetFace(e)
    }
}

impl From<codepoint_resolver::RenderGlyphError> for RenderGlyphError {
    fn from(e: codepoint_resolver::RenderGlyphError) -> RenderGlyphError {
        RenderGlyphError::Render(e)
    }
}

impl From<atlas::AllocError> for RenderGlyphError {
    fn from(_: atlas::AllocError) -> RenderGlyphError {
        RenderGlyphError::OutOfMemory
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Render {
    pub glyph: Glyph,
    pub presentation: Presentation,
}

/// Lock helpers that ignore poisoning (Zig locks have no poisoning).
fn read(lock: &RwLock<State>) -> RwLockReadGuard<'_, State> {
    lock.read().unwrap_or_else(|e| e.into_inner())
}

fn write(lock: &RwLock<State>) -> RwLockWriteGuard<'_, State> {
    lock.write().unwrap_or_else(|e| e.into_inner())
}

impl SharedGrid {
    /// Initialize the grid.
    ///
    /// The resolver must have a collection that supports deferred loading
    /// (collection.load_options != null). This is because we need the load
    /// options data to determine grid metrics and setup our sprite font.
    ///
    /// SharedGrid always configures the sprite font. This struct is expected to be
    /// used with a terminal grid and therefore the sprite font is always
    /// necessary for correct rendering.
    pub fn init(resolver: CodepointResolver) -> Result<SharedGrid, InitError> {
        let tw = &INIT_TW;

        // We need to support loading options since we use the size data
        crate::quirks::inline_assert(resolver.collection.load_options.is_some());

        let atlas_grayscale = Atlas::init(512, atlas::Format::Grayscale)?;
        let atlas_color = Atlas::init(512, atlas::Format::Bgra)?;

        let mut state = State {
            codepoints: HashMap::default(),
            glyphs: HashMap::default(),
            atlas_grayscale,
            atlas_color,
            resolver,
        };

        // We set an initial capacity that can fit a good number of characters.
        // This number was picked empirically based on my own terminal usage.
        tw.check(InitFailPoint::CodepointsCapacity)?;
        state.codepoints.reserve(128);
        tw.check(InitFailPoint::GlyphsCapacity)?;
        state.glyphs.reserve(128);

        // Initialize our metrics.
        tw.check(InitFailPoint::ReloadMetrics)?;
        let metrics = Self::reload_metrics(&mut state)?;

        Ok(SharedGrid {
            metrics,
            lock: RwLock::new(state),
        })
    }

    fn reload_metrics(state: &mut State) -> Result<Metrics, collection::UpdateMetricsError> {
        let collection = &mut state.resolver.collection;
        collection.update_metrics()?;

        let metrics = collection.metrics.unwrap();

        // Setup our sprite font.
        state.resolver.sprite = Some(crate::font::sprite::Face { metrics });

        Ok(metrics)
    }

    /// Returns the grid cell size.
    ///
    /// This is not thread safe.
    pub fn cell_size(&self) -> CellSize {
        CellSize {
            width: self.metrics.cell_width,
            height: self.metrics.cell_height,
        }
    }

    /// Get the font index for a given codepoint. This is cached.
    ///
    /// This always forces loading any deferred fonts since we assume that if
    /// you're looking up an index that the caller plans to use the font. By
    /// loading the font in this function we can ensure thread-safety on the
    /// load without complicating future calls.
    pub fn get_index(
        &self,
        cp: u32,
        style: Style,
        p: Option<Presentation>,
    ) -> Result<Option<Index>, GetFaceError> {
        let key = CodepointKey::from(style, cp, p);

        // Fast path: the cache has the value. This is almost always true and
        // only requires a read lock.
        {
            let state = read(&self.lock);
            if let Some(v) = state.codepoints.get(&key) {
                return Ok(*v);
            }
        }

        // Slow path: we need to search this codepoint
        let mut guard = write(&self.lock);
        let state = &mut *guard;

        // Try to get it, if it is now in the cache another thread beat us to it.
        if let Some(v) = state.codepoints.get(&key) {
            return Ok(*v);
        }

        // Load a value and cache it. This even caches negative matches.
        let value = state.resolver.get_index(cp, style, p);
        state.codepoints.insert(key, value);

        if let Some(idx) = value {
            // If the font is a sprite font then we don't need to preload
            // because getFace doesn't work with special fonts.
            if idx.special().is_none() {
                // Load the face in case its deferred. If this fails then we would've
                // failed to load it in the future anyways so we want to undo all
                // the caching we did.
                if let Err(err) = state.resolver.collection.get_face(idx) {
                    state.codepoints.remove(&key);
                    return Err(err);
                }
            }
        }

        Ok(value)
    }

    /// Returns true if the given font index has the codepoint and presentation.
    pub fn has_codepoint(&self, idx: Index, cp: u32, p: Option<Presentation>) -> bool {
        let state = read(&self.lock);
        state.resolver.collection.has_codepoint(
            idx,
            cp,
            match p {
                Some(v) => PresentationMode::Explicit(v),
                None => PresentationMode::Any,
            },
        )
    }

    /// Render a codepoint. This uses the first font index that has the codepoint
    /// and matches the presentation requested. If the codepoint cannot be found
    /// in any font, an null render is returned.
    pub fn render_codepoint(
        &self,
        cp: u32,
        style: Style,
        p: Option<Presentation>,
        opts: RenderOptions,
    ) -> Result<Option<Render>, RenderGlyphError> {
        // Note: we could optimize the below to use way less locking, but
        // at the time of writing this codepath is only called for preedit
        // text which is relatively rare and almost non-existent in multiple
        // surfaces at the same time.

        // Get the font that has the codepoint
        let Some(index) = self.get_index(cp, style, p)? else {
            return Ok(None);
        };

        // Get the glyph for the font
        let glyph_index = {
            // NOTE: the Zig code takes a shared lock here; `getFace` needs
            // mutable access in Rust, so this takes the exclusive lock.
            let mut state = write(&self.lock);
            let face = state.resolver.collection.get_face(index)?;
            let Some(glyph_index) = face.glyph_index(cp) else {
                return Ok(None);
            };
            glyph_index
        };

        // Render
        Ok(Some(self.render_glyph(index, glyph_index, opts)?))
    }

    /// Render a glyph index. This automatically determines the correct texture
    /// atlas to use and caches the result.
    pub fn render_glyph(
        &self,
        index: Index,
        glyph_index: u32,
        opts: RenderOptions,
    ) -> Result<Render, RenderGlyphError> {
        let tw = &RENDER_GLYPH_TW;

        let key = GlyphKey::from(index, glyph_index, &opts);

        // Fast path: the cache has the value. This is almost always true and
        // only requires a read lock.
        {
            let state = read(&self.lock);
            if let Some(v) = state.glyphs.get(&key) {
                return Ok(*v);
            }
        }

        // Slow path: we need to search this codepoint
        let mut guard = write(&self.lock);
        let state = &mut *guard;

        if let Some(v) = state.glyphs.get(&key) {
            return Ok(*v);
        }
        // NOTE: the Zig code inserts an undefined entry here (getOrPut)
        // and removes it on error (errdefer). We only insert on success,
        // which leaves the cache in the same state.

        // Get the presentation to determine what atlas to use
        tw.check(RenderGlyphFailPoint::GetPresentation)?;
        let p = state.resolver.get_presentation(index, glyph_index)?;
        let atlas: &mut Atlas = match p {
            Presentation::Text => &mut state.atlas_grayscale,
            Presentation::Emoji => &mut state.atlas_color,
        };

        let mut render_opts = opts;

        // Always use these constraints for emoji.
        if p == Presentation::Emoji {
            render_opts.constraint = Constraint {
                // Scale emoji to be as large as possible
                // while preserving their aspect ratio.
                size: ConstraintSize::Cover,

                // Center the emoji in its cells.
                align_horizontal: ConstraintAlign::Center,
                align_vertical: ConstraintAlign::Center,

                // Add a small bit of padding so the emoji
                // doesn't quite touch the edges of the cells.
                pad_left: 0.025,
                pad_right: 0.025,

                ..Constraint::NONE
            };
        }

        // Render into the atlas
        let glyph = match state.resolver.render_glyph(atlas, index, glyph_index, render_opts) {
            Ok(glyph) => glyph,

            // If the atlas is full, we resize it
            Err(err) if err.is_atlas_full() => {
                let size = atlas.size;
                atlas.grow(size * 2)?;
                state.resolver.render_glyph(atlas, index, glyph_index, render_opts)?
            }

            Err(err) => return Err(err.into()),
        };

        // Cache and return
        let render = Render {
            glyph,
            presentation: p,
        };
        state.glyphs.insert(key, render);

        Ok(render)
    }
}

/// Hasher for the packed `u64` cache keys: `x ^ (x >> 32)` (the Zig
/// `Context.hash`).
#[derive(Debug, Default, Clone, Copy)]
pub struct KeyHasher(u64);

impl Hasher for KeyHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        // Only u64 keys are hashed; fold any other input in for safety.
        for b in bytes {
            self.0 = self.0.rotate_left(8) ^ (*b as u64);
        }
    }

    fn write_u64(&mut self, x: u64) {
        self.0 = x ^ (x >> 32);
    }
}

pub type KeyHashBuilder = BuildHasherDefault<KeyHasher>;

/// Zig `packed struct(u64)`:
/// `codepoint: u32, style: u3, has_presentation: bool, presentation: u1, _padding: u27`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CodepointKey(pub u64);

impl CodepointKey {
    #[inline]
    pub fn from(style: Style, codepoint: u32, presentation: Option<Presentation>) -> CodepointKey {
        let has_presentation = presentation.is_some();
        let presentation = presentation.unwrap_or(Presentation::Text);
        CodepointKey(
            (codepoint as u64)
                | ((style as u64) << 32)
                | ((has_presentation as u64) << 35)
                | ((presentation as u64) << 36),
        )
    }
}

/// Cache key for rendered glyphs. Packed to 8 bytes so HashMap stores
/// and compares a u64 instead of a full RenderOptions (metrics,
/// nerd-font constraint, etc. are not part of the identity).
///
/// Zig `packed struct(u64)`: `glyph: u32, index: Collection.Index (u16),
/// opts: packed struct(u16) { cell_width: u2, thicken: bool,
/// thicken_strength: u8, constraint_width: u2, _padding: u3 }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GlyphKey(pub u64);

impl GlyphKey {
    #[inline]
    pub fn from(index: Index, glyph: u32, opts: &RenderOptions) -> GlyphKey {
        let packed_opts: u64 = ((opts.cell_width.unwrap_or(0) as u64) & 0b11)
            | ((opts.thicken as u64) << 2)
            | ((opts.thicken_strength as u64) << 3)
            | (((opts.constraint_width as u64) & 0b11) << 11);
        GlyphKey((glyph as u64) | ((index.int() as u64) << 32) | (packed_opts << 48))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::font::collection::{AddOptions, Collection, LoadOptions, SizeAdjustment};
    use crate::font::face::{DesiredSize, Face};
    use crate::font::embedded;
    use crate::font::library::Library;

    #[derive(Debug, Clone, Copy)]
    enum TestMode {
        Normal,
    }

    fn size96() -> DesiredSize {
        DesiredSize {
            points: 12.0,
            xdpi: 96,
            ydpi: 96,
        }
    }

    fn test_grid(mode: TestMode, lib: &Library) -> Result<SharedGrid, InitError> {
        let test_font = embedded::REGULAR;

        let mut c = Collection::init();
        c.load_options = Some(LoadOptions::new(lib.clone()));

        match mode {
            TestMode::Normal => {
                c.add(
                    Face::init(lib.clone(), test_font, size96().into()).unwrap(),
                    AddOptions {
                        style: Style::Regular,
                        fallback: false,
                        size_adjustment: SizeAdjustment::None,
                    },
                )
                .unwrap();
            }
        }

        let r = CodepointResolver::new(c);

        SharedGrid::init(r)
    }

    #[test]
    fn get_index() {
        // const testEmoji = @import("test.zig").fontEmoji;

        let mut lib = Library::init().unwrap();
        {
            let grid = test_grid(TestMode::Normal, &lib).unwrap();

            // Visible ASCII.
            for i in 32u32..127 {
                let idx = grid.get_index(i, Style::Regular, None).unwrap().unwrap();
                assert_eq!(Style::Regular, idx.style);
                assert_eq!(0, idx.idx);
                assert!(grid.has_codepoint(idx, i, None));
            }

            // Do it again without a resolver set to ensure we only hit the cache
            // (the Zig test sets the resolver to `undefined`; we swap in an
            // empty one that can't resolve anything).
            let old_resolver = std::mem::replace(
                &mut grid.lock.write().unwrap().resolver,
                CodepointResolver::new(Collection::init()),
            );
            for i in 32u32..127 {
                let idx = grid.get_index(i, Style::Regular, None).unwrap().unwrap();
                assert_eq!(Style::Regular, idx.style);
                assert_eq!(0, idx.idx);
            }
            grid.lock.write().unwrap().resolver = old_resolver;
        }
        lib.deinit();
    }

    #[test]
    fn render_glyph_error_after_cache_insert_rolls_back_cache_entry() {
        // This test verifies that when renderGlyph fails after inserting a cache
        // entry (via getOrPut), the errdefer properly removes the entry, preventing
        // corrupted/uninitialized data from remaining in the cache.

        let mut lib = Library::init().unwrap();
        {
            let grid = test_grid(TestMode::Normal, &lib).unwrap();

            // Get the font index for 'A'
            let idx = grid.get_index('A' as u32, Style::Regular, None).unwrap().unwrap();

            // Get the glyph index for 'A'
            let glyph_index = {
                let mut state = grid.lock.write().unwrap();
                let face = state.resolver.collection.get_face(idx).unwrap();
                face.glyph_index('A' as u32).unwrap()
            };

            let render_opts = RenderOptions::new(grid.metrics);
            let key = GlyphKey::from(idx, glyph_index, &render_opts);

            // Verify the cache is empty for this glyph
            assert!(grid.lock.read().unwrap().glyphs.get(&key).is_none());

            // Set up tripwire to fail after cache insert.
            // We use OutOfMemory as it's a valid error in the renderGlyph error set.
            let tw = &RENDER_GLYPH_TW;
            tw.error_always(RenderGlyphFailPoint::GetPresentation, RenderGlyphError::OutOfMemory);

            // This should fail due to the tripwire
            assert_eq!(
                Err(RenderGlyphError::OutOfMemory),
                grid.render_glyph(idx, glyph_index, render_opts)
            );
            let _ = tw.end(tripwire::ResetMode::Reset);

            // The errdefer should have removed the cache entry, leaving the cache clean.
            // Without the errdefer fix, this would contain garbage/uninitialized data.
            assert!(grid.lock.read().unwrap().glyphs.get(&key).is_none());
        }
        lib.deinit();
    }

    #[test]
    fn init_error() {
        // Test every failure point in `init` and ensure that we don't
        // leak memory (testing.allocator verifies) since we're exiting early.
        for tag in InitFailPoint::ALL {
            let tw = &INIT_TW;
            tw.error_always(tag, InitError::OutOfMemory);

            // Create a resolver for testing - we need to set up a minimal one.
            let mut lib = Library::init().unwrap();

            {
                let mut c = Collection::init();
                c.load_options = Some(LoadOptions::new(lib.clone()));
                c.add(
                    Face::init(lib.clone(), embedded::REGULAR, size96().into()).unwrap(),
                    AddOptions {
                        style: Style::Regular,
                        fallback: false,
                        size_adjustment: SizeAdjustment::None,
                    },
                )
                .unwrap();

                let resolver = CodepointResolver::new(c);

                // In Rust the resolver is dropped by `init` on failure.
                assert_eq!(Err(InitError::OutOfMemory), SharedGrid::init(resolver).map(|_| ()));
            }

            tw.end(tripwire::ResetMode::Reset).unwrap();
            lib.deinit();
        }
    }
}
