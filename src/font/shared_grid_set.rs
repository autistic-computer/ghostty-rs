//! Port of `src/font/SharedGridSet.zig`.
//!
//! STATUS: ported
//!
//! This structure contains a set of SharedGrid structures keyed by
//! unique font configuration.
//!
//! Most terminals (surfaces) will share the same font configuration.
//! This structure allows expensive font information such as
//! the font atlas, glyph cache, font faces, etc. to be shared.
//!
//! This structure is thread-safe when the operations are documented
//! as thread-safe.
//!
//! Port notes:
//! * Grids are handed out as `Arc<SharedGrid>` (Zig: `*SharedGrid`); the
//!   explicit ref count is kept so a grid is removed from the set exactly
//!   when the Zig code would deinit it. As in Zig, the grids must not be
//!   used after the set is dropped (the font library is deinitialized).
//! * The map is keyed by `Key.hashcode()` (the Zig map compares keys by
//!   hash only) and stores the key next to the grid.
//! * The font discovery handle is an `Arc` shared with the resolvers.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::config::{
    Config, FontStyle, FontSyntheticStyle, RepeatableCodepointMap, RepeatableFontVariation,
    RepeatableString,
};
use crate::font::codepoint_map::CodepointMap;
use crate::font::codepoint_resolver::{CodepointResolver, StyleStatus};
use crate::font::collection::{self, AddOptions, Collection, SizeAdjustment};
#[allow(unused_imports)]
use crate::font::deferred_face;
use crate::font::discovery::{self, Discover};
use crate::font::face::{self, DesiredSize, Face, FreetypeLoadFlags, Variation, VariationId};
use crate::font::library::Library;
use crate::font::metrics::{self, Metrics};
use crate::font::shared_grid::{self, SharedGrid};
use crate::font::{embedded, Style};
use crate::terminal::hash_map::Wyhash;

const LOG: &str = "font_shared_grid_set";

pub struct SharedGridSet {
    /// The map of font configurations to SharedGrid instances.
    ///
    /// Lock to protect multi-threaded access to the map (Zig `lock`).
    pub map: Mutex<Map>,

    /// The font library that is used for all font groups.
    pub font_lib: Library,

    /// Font discovery mechanism.
    pub font_discover: Option<Arc<Discover>>,
}

// SAFETY: the discovery handle is shared with the grids' resolvers exactly
// like the Zig code shares its pointer; the map is protected by its mutex.
unsafe impl Send for SharedGridSet {}
unsafe impl Sync for SharedGridSet {}

/// Zig `InitError` (`Library.InitError`).
#[cfg(not(any(target_os = "macos", target_os = "ios", target_arch = "wasm32")))]
pub type InitError = crate::font::library::FreetypeInitError;

/// Zig `InitError` (`Library.InitError`).
#[cfg(any(target_os = "macos", target_os = "ios", target_arch = "wasm32"))]
pub type InitError = crate::font::library::NoopInitError;

/// The (inferred) error set of `ref` / `collection`.
#[derive(Debug, Clone, PartialEq)]
pub enum RefError {
    /// Font discovery failed (the error is formatted for logging).
    Discovery(String),
    /// A deferred face operation (e.g. `name`) failed.
    DeferredFace(deferred_face::Error),
    /// Initializing or modifying a face failed.
    Face(collection::FaceError),
    Add(collection::AddError),
    AddDeferred(collection::AddDeferredError),
    Complete(collection::CompleteError),
    GetFace(collection::GetFaceError),
    Init(shared_grid::InitError),
}

impl std::fmt::Display for RefError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RefError::Discovery(e) => f.write_str(e),
            other => std::fmt::Debug::fmt(other, f),
        }
    }
}

impl std::error::Error for RefError {}

macro_rules! ref_error_from {
    ($($t:ty => $v:ident),* $(,)?) => {
        $(impl From<$t> for RefError {
            fn from(e: $t) -> RefError {
                RefError::$v(e)
            }
        })*
    };
}

ref_error_from! {
    deferred_face::Error => DeferredFace,
    collection::FaceError => Face,
    collection::AddError => Add,
    collection::AddDeferredError => AddDeferred,
    collection::CompleteError => Complete,
    collection::GetFaceError => GetFace,
    shared_grid::InitError => Init,
}

fn lock(m: &Mutex<Map>) -> MutexGuard<'_, Map> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl SharedGridSet {
    /// Initialize a new SharedGridSet.
    pub fn init() -> Result<SharedGridSet, InitError> {
        let font_lib = Library::init()?;

        Ok(SharedGridSet {
            map: Mutex::new(Map::new()),
            font_lib,
            font_discover: None,
        })
    }

    /// Returns the number of cached grids.
    pub fn count(&self) -> usize {
        lock(&self.map).len()
    }

    /// Initialize a SharedGrid for the given font configuration. If the
    /// SharedGrid is not present it will be initialized with a ref count of
    /// 1. If it is present, the ref count will be incremented.
    ///
    /// This is NOT thread-safe.
    ///
    /// The returned data (key and grid) should never be freed. The memory is
    /// owned by the set and will be freed when the ref count reaches zero.
    pub fn r#ref(
        &mut self,
        config: &DerivedConfig,
        font_size: DesiredSize,
    ) -> Result<(Key, Arc<SharedGrid>), RefError> {
        let key = Key::init(config, font_size);
        let hash = key.hashcode();

        {
            let mut map = lock(&self.map);
            if let Some(entry) = map.get_mut(&hash) {
                log::debug!(target: LOG, "found cached grid for font config");

                // Increment our ref count and return the cache
                // (the freshly built key is dropped since we found a
                // cached value).
                entry.value.r#ref += 1;
                return Ok((entry.key.clone(), entry.value.grid.clone()));
            }
        }

        log::debug!(target: LOG, "initializing new grid for font config");

        // A new font config, initialize the cache.
        let resolver = {
            // Build our collection. This is the expensive operation that
            // involves finding fonts, loading them (maybe, some are deferred),
            // etc.
            let c = self.collection(&key, font_size, config)?;

            // Setup our enabled/disabled styles
            let mut styles = StyleStatus::init_fill(true);
            styles.set(Style::Bold, config.font_style_bold != FontStyle::False);
            styles.set(Style::Italic, config.font_style_italic != FontStyle::False);
            styles.set(Style::BoldItalic, config.font_style_bold_italic != FontStyle::False);

            // Init our resolver which just requires setting fields.
            let mut r = CodepointResolver::new(c);
            r.styles = styles;
            r.discover = self.discover();
            r.codepoint_map = Some(key.codepoint_map.clone());
            r
        };
        let grid = Arc::new(SharedGrid::init(resolver)?);

        let mut map = lock(&self.map);
        map.insert(
            hash,
            MapEntry {
                key: key.clone(),
                value: ReffedGrid {
                    grid: grid.clone(),
                    r#ref: 1,
                },
            },
        );

        Ok((key, grid))
    }

    /// Builds the Collection for the given configuration key and
    /// initial font size.
    fn collection(
        &mut self,
        key: &Key,
        size: DesiredSize,
        config: &DerivedConfig,
    ) -> Result<Collection, RefError> {
        // A quick note on memory management:
        // - font_lib is owned by the SharedGridSet
        // - metric_modifiers is owned by the key which is freed only when
        //   the ref count for this grid reaches zero.
        let load_options = collection::LoadOptions {
            library: self.font_lib.clone(),
            size,
            freetype_load_flags: key.freetype_load_flags,
        };

        let mut c = Collection::init();
        c.load_options = Some(load_options.clone());
        c.metric_modifiers = key.metric_modifiers.clone();

        // Search for fonts
        #[cfg(not(target_arch = "wasm32"))]
        'discover: {
            let Some(disco) = self.discover() else {
                log::warn!(target: LOG, "font discovery not available, cannot search for fonts");
                break 'discover;
            };

            // A buffer we use to store the font names for logging.
            let mut name_buf = [0u8; 256];

            for style in Style::ALL {
                let field = style.as_str();
                for desc in key.descriptors_for_style(style) {
                    {
                        let mut disco_it = disco
                            .discover(desc)
                            .map_err(|e| RefError::Discovery(format!("{e:?}")))?;
                        if let Some(face) = disco_it
                            .next()
                            .map_err(|e| RefError::Discovery(format!("{e:?}")))?
                        {
                            log::info!(
                                target: LOG,
                                "font {}: {}",
                                field,
                                String::from_utf8_lossy(face.name(&mut name_buf)?),
                            );

                            c.add_deferred(
                                face,
                                AddOptions {
                                    style,
                                    fallback: false,
                                    // No size adjustment for primary fonts.
                                    size_adjustment: SizeAdjustment::None,
                                },
                            )?;

                            continue;
                        }
                    }

                    // If there are variation configurations and we didn't find
                    // the font, then we retry the discovery with all stylistic
                    // bits set to false. This is because some fonts may not
                    // set the stylistic bit in their table but still support
                    // axes to mimic the style. At the time of writing, Berkeley
                    // Mono Variable is like this. See #2140.
                    if style != Style::Regular && !desc.variations.is_empty() {
                        let mut copy = desc.clone();
                        copy.bold = false;
                        copy.italic = false;
                        let mut disco_it = disco
                            .discover(&copy)
                            .map_err(|e| RefError::Discovery(format!("{e:?}")))?;
                        if let Some(face) = disco_it
                            .next()
                            .map_err(|e| RefError::Discovery(format!("{e:?}")))?
                        {
                            log::info!(
                                target: LOG,
                                "font {}: {}",
                                field,
                                String::from_utf8_lossy(face.name(&mut name_buf)?),
                            );

                            c.add_deferred(
                                face,
                                AddOptions {
                                    style,
                                    fallback: false,
                                    // No size adjustment for primary fonts.
                                    size_adjustment: SizeAdjustment::None,
                                },
                            )?;

                            continue;
                        }
                    }

                    log::warn!(
                        target: LOG,
                        "font-family {} not found: {}",
                        field,
                        desc.family.as_deref().unwrap(),
                    );
                }
            }
        }

        // Complete our styles to ensure we have something to satisfy every
        // possible style request. We do this before adding our built-in font
        // because we want to ensure our built-in styles are fallbacks to
        // the configured styles.
        c.complete_styles(config.font_synthetic_style)?;

        let fallback_opts = |style: Style, size_adjustment: SizeAdjustment| AddOptions {
            style,
            fallback: true,
            size_adjustment,
        };
        let wght_700 = [Variation {
            id: VariationId::init(b"wght"),
            value: 700.0,
        }];

        // Our built-in font will be used as a backup
        c.add(
            Face::init(self.font_lib.clone(), embedded::VARIABLE, load_options.face_options())?,
            fallback_opts(Style::Regular, crate::font::DEFAULT_FALLBACK_ADJUSTMENT),
        )?;
        {
            let idx = c.add(
                Face::init(self.font_lib.clone(), embedded::VARIABLE, load_options.face_options())?,
                fallback_opts(Style::Bold, crate::font::DEFAULT_FALLBACK_ADJUSTMENT),
            )?;
            c.get_face(idx)?
                .set_variations(&wght_700, load_options.face_options())
                .map_err(|e| RefError::Face(collection::FaceError::from(e)))?;
        }
        c.add(
            Face::init(self.font_lib.clone(), embedded::VARIABLE_ITALIC, load_options.face_options())?,
            fallback_opts(Style::Italic, crate::font::DEFAULT_FALLBACK_ADJUSTMENT),
        )?;
        {
            let idx = c.add(
                Face::init(self.font_lib.clone(), embedded::VARIABLE_ITALIC, load_options.face_options())?,
                fallback_opts(Style::BoldItalic, crate::font::DEFAULT_FALLBACK_ADJUSTMENT),
            )?;
            c.get_face(idx)?
                .set_variations(&wght_700, load_options.face_options())
                .map_err(|e| RefError::Face(collection::FaceError::from(e)))?;
        }

        // Nerd-font symbols fallback.
        c.add(
            Face::init(self.font_lib.clone(), embedded::SYMBOLS_NERD_FONT, load_options.face_options())?,
            // No size adjustment for the symbols font.
            fallback_opts(Style::Regular, SizeAdjustment::None),
        )?;

        // On macOS, always search for and add the Apple Emoji font
        // as our preferred emoji font for fallback. We do this in case
        // people add other emoji fonts to their system, we always want to
        // prefer the official one. Users can override this by explicitly
        // specifying a font-family for emoji.
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        'apple_emoji: {
            let Some(disco) = self.discover() else {
                break 'apple_emoji;
            };

            // Fast path: we know the exact name of the font we want so we
            // can look it up directly, which is sometimes significantly faster than
            // full discovery (e.g. CoreText).
            if let Some(face) = disco
                .discover_exact_family(b"Apple Color Emoji")
                .map_err(|e| RefError::Discovery(format!("{e:?}")))?
            {
                c.add_deferred(
                    face,
                    // No size adjustment for emojis.
                    fallback_opts(Style::Regular, SizeAdjustment::None),
                )?;
                break 'apple_emoji;
            }

            let mut disco_it = disco
                .discover(&discovery::Descriptor {
                    family: Some("Apple Color Emoji".to_string()),
                    ..Default::default()
                })
                .map_err(|e| RefError::Discovery(format!("{e:?}")))?;
            if let Some(face) = disco_it
                .next()
                .map_err(|e| RefError::Discovery(format!("{e:?}")))?
            {
                c.add_deferred(
                    face,
                    // No size adjustment for emojis.
                    fallback_opts(Style::Regular, SizeAdjustment::None),
                )?;
            }
        }

        // Emoji fallback. We don't include this on Mac since Mac is expected
        // to always have the Apple Emoji available on the system.
        #[cfg(not(any(target_os = "macos", target_os = "ios")))]
        {
            c.add(
                Face::init(self.font_lib.clone(), embedded::EMOJI, load_options.face_options())?,
                // No size adjustment for emojis.
                fallback_opts(Style::Regular, SizeAdjustment::None),
            )?;
            c.add(
                Face::init(self.font_lib.clone(), embedded::EMOJI_TEXT, load_options.face_options())?,
                // No size adjustment for emojis.
                fallback_opts(Style::Regular, SizeAdjustment::None),
            )?;
        }

        Ok(c)
    }

    /// Decrement the ref count for the given key. If the ref count is zero,
    /// the grid will be deinitialized and removed from the map.j:w
    pub fn deref(&self, key: &Key) {
        let mut map = lock(&self.map);

        let hash = key.hashcode();
        let Some(entry) = map.get_mut(&hash) else {
            return;
        };
        crate::quirks::inline_assert(entry.value.r#ref >= 1);

        // If we have more than one reference, decrement and return.
        if entry.value.r#ref > 1 {
            entry.value.r#ref -= 1;
            return;
        }

        // We are at a zero ref count so deinit the group and remove.
        map.remove(&hash);
    }

    /// Initialize once and return the font discovery mechanism. This remains
    /// initialized throughout the lifetime of the application because some
    /// font discovery mechanisms (i.e. fontconfig) are unsafe to reinit.
    fn discover(&mut self) -> Option<Arc<Discover>> {
        // If we're built without a font discovery mechanism, return null
        #[cfg(target_arch = "wasm32")]
        {
            None
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            // If we initialized, use it
            if let Some(v) = &self.font_discover {
                return Some(v.clone());
            }

            let v = Arc::new(Discover::init(&self.font_lib));
            self.font_discover = Some(v.clone());
            Some(v)
        }
    }
}

impl Drop for SharedGridSet {
    /// Zig `deinit`.
    fn drop(&mut self) {
        // Deinit all grids (and their faces) before the font library.
        lock(&self.map).clear();
        self.font_discover = None;
        self.font_lib.deinit();
    }
}

/// Map of font configurations to grid instances. The grid
/// instances are pointers that are heap allocated so that they're
/// stable pointers across hash map resizes.
///
/// Keyed by `Key.hashcode()`: the Zig map's `eql` compares hashes.
pub type Map = HashMap<u64, MapEntry>;

/// A map entry (the Zig map's key and value).
pub struct MapEntry {
    pub key: Key,
    pub value: ReffedGrid,
}

/// Ref-counted SharedGrid.
pub struct ReffedGrid {
    pub grid: Arc<SharedGrid>,
    pub r#ref: u32,
}

/// This is the configuration required to create a key without having
/// to keep the full Ghostty configuration around.
#[derive(Debug, Clone)]
pub struct DerivedConfig {
    pub font_family: RepeatableString,
    pub font_family_bold: RepeatableString,
    pub font_family_italic: RepeatableString,
    pub font_family_bold_italic: RepeatableString,
    pub font_style: FontStyle,
    pub font_style_bold: FontStyle,
    pub font_style_italic: FontStyle,
    pub font_style_bold_italic: FontStyle,
    pub font_variation: RepeatableFontVariation,
    pub font_variation_bold: RepeatableFontVariation,
    pub font_variation_italic: RepeatableFontVariation,
    pub font_variation_bold_italic: RepeatableFontVariation,
    pub font_codepoint_map: RepeatableCodepointMap,
    pub font_synthetic_style: FontSyntheticStyle,
    pub adjust_cell_width: Option<metrics::Modifier>,
    pub adjust_cell_height: Option<metrics::Modifier>,
    pub adjust_font_baseline: Option<metrics::Modifier>,
    pub adjust_underline_position: Option<metrics::Modifier>,
    pub adjust_underline_thickness: Option<metrics::Modifier>,
    pub adjust_strikethrough_position: Option<metrics::Modifier>,
    pub adjust_strikethrough_thickness: Option<metrics::Modifier>,
    pub adjust_overline_position: Option<metrics::Modifier>,
    pub adjust_overline_thickness: Option<metrics::Modifier>,
    pub adjust_cursor_thickness: Option<metrics::Modifier>,
    pub adjust_cursor_height: Option<metrics::Modifier>,
    pub adjust_box_thickness: Option<metrics::Modifier>,
    pub adjust_icon_height: Option<metrics::Modifier>,
    pub freetype_load_flags: FreetypeLoadFlags,
}

impl DerivedConfig {
    /// Initialize a DerivedConfig from a config.Config. (The Zig `init`
    /// also accepts another DerivedConfig to clone from; that is
    /// `Clone::clone` here.)
    pub fn init(config: &Config) -> DerivedConfig {
        DerivedConfig {
            font_family: config.font_family.clone(),
            font_family_bold: config.font_family_bold.clone(),
            font_family_italic: config.font_family_italic.clone(),
            font_family_bold_italic: config.font_family_bold_italic.clone(),
            font_style: config.font_style.clone(),
            font_style_bold: config.font_style_bold.clone(),
            font_style_italic: config.font_style_italic.clone(),
            font_style_bold_italic: config.font_style_bold_italic.clone(),
            font_variation: config.font_variation.clone(),
            font_variation_bold: config.font_variation_bold.clone(),
            font_variation_italic: config.font_variation_italic.clone(),
            font_variation_bold_italic: config.font_variation_bold_italic.clone(),
            font_codepoint_map: config.font_codepoint_map.clone(),
            font_synthetic_style: config.font_synthetic_style,
            adjust_cell_width: config.adjust_cell_width,
            adjust_cell_height: config.adjust_cell_height,
            adjust_font_baseline: config.adjust_font_baseline,
            adjust_underline_position: config.adjust_underline_position,
            adjust_underline_thickness: config.adjust_underline_thickness,
            adjust_strikethrough_position: config.adjust_strikethrough_position,
            adjust_strikethrough_thickness: config.adjust_strikethrough_thickness,
            adjust_overline_position: config.adjust_overline_position,
            adjust_overline_thickness: config.adjust_overline_thickness,
            adjust_cursor_thickness: config.adjust_cursor_thickness,
            adjust_cursor_height: config.adjust_cursor_height,
            adjust_box_thickness: config.adjust_box_thickness,
            adjust_icon_height: config.adjust_icon_height,
            freetype_load_flags: config.freetype_load_flags,
        }
    }
}

/// The number of style offsets (`std.enums.directEnumArrayLen(Style, 0)`).
pub const STYLE_OFFSETS_LEN: usize = 4;
pub type StyleOffsets = [usize; STYLE_OFFSETS_LEN];

// We assume this throughout this structure. If this changes
// we may need to change this structure.
const _: () = {
    assert!(Style::Regular as u8 == 0);
    assert!(Style::Bold as u8 == 1);
    assert!(Style::Italic as u8 == 2);
    assert!(Style::BoldItalic as u8 == 3);
};

/// The key used to uniquely identify a font configuration.
#[derive(Debug, Clone)]
pub struct Key {
    /// The descriptors used for all the fonts added to the
    /// initial group, including all styles. This is hashed
    /// in order so the order matters. All users of the struct
    /// should ensure that the order is consistent.
    pub descriptors: Vec<discovery::Descriptor>,

    /// These are the offsets into the descriptors array for
    /// each style. For example, bold is from
    /// offsets[@backingInt(.bold) - 1] to
    /// offsets[@backingInt(.bold)].
    pub style_offsets: StyleOffsets,

    /// The codepoint map configuration.
    pub codepoint_map: CodepointMap,

    /// The metric modifier set configuration.
    pub metric_modifiers: metrics::ModifierSet,

    /// The configured font size for this key. We don't use this
    /// directly but it is used as part of the hash for the
    /// font grid.
    pub font_size: DesiredSize,

    /// The freetype load flags configuration, only non-void if the
    /// freetype backend is enabled.
    pub freetype_load_flags: FreetypeLoadFlags,
}

impl Default for Key {
    fn default() -> Key {
        Key {
            descriptors: Vec::new(),
            style_offsets: [0; STYLE_OFFSETS_LEN],
            codepoint_map: CodepointMap::default(),
            metric_modifiers: metrics::ModifierSet::default(),
            font_size: DesiredSize::new(12.0),
            freetype_load_flags: face::freetype_load_flags_default(),
        }
    }
}

impl Key {
    pub fn init(config_src: &DerivedConfig, font_size: DesiredSize) -> Key {
        // Clone our configuration. We need to do this because the lifetime
        // of the derived config is usually shorter than that of a key
        // and we use pointers into the derived config for the key. We
        // can remove this if we wanted by dupe-ing the memory we use
        // from DerivedConfig below.
        let config = config_src.clone();

        let name_value = |s: &FontStyle| s.name_value().map(|v| v.as_str().to_string());
        let family = |f: &crate::cli::args::StringZ| Some(f.as_str().to_string());

        let mut descriptors: Vec<discovery::Descriptor> = Vec::new();
        for f in &config.font_family.list {
            descriptors.push(discovery::Descriptor {
                family: family(f),
                style: name_value(&config.font_style),
                size: font_size.points,
                variations: config.font_variation.list.clone(),
                ..Default::default()
            });
        }

        // In all the styled cases below, we prefer to specify an exact
        // style via the `font-style` configuration. If a style is not
        // specified, we use the discovery mechanism to search for a
        // style category such as bold, italic, etc. We can't specify both
        // because the latter will restrict the search to only that. If
        // a user says `font-style = italic` for the bold face for example,
        // no results would be found if we restrict to ALSO searching for
        // italic.
        for f in &config.font_family_bold.list {
            let style = name_value(&config.font_style_bold);
            descriptors.push(discovery::Descriptor {
                family: family(f),
                bold: style.is_none(),
                style,
                size: font_size.points,
                variations: config.font_variation_bold.list.clone(),
                ..Default::default()
            });
        }
        for f in &config.font_family_italic.list {
            let style = name_value(&config.font_style_italic);
            descriptors.push(discovery::Descriptor {
                family: family(f),
                italic: style.is_none(),
                style,
                size: font_size.points,
                variations: config.font_variation_italic.list.clone(),
                ..Default::default()
            });
        }
        for f in &config.font_family_bold_italic.list {
            let style = name_value(&config.font_style_bold_italic);
            descriptors.push(discovery::Descriptor {
                family: family(f),
                bold: style.is_none(),
                italic: style.is_none(),
                style,
                size: font_size.points,
                variations: config.font_variation_bold_italic.list.clone(),
                ..Default::default()
            });
        }

        // Setup the codepoint map
        let codepoint_map: CodepointMap = {
            let map = &config.font_codepoint_map;
            if map.map.is_empty() {
                CodepointMap::default()
            } else {
                map.map.clone_map()
            }
        };

        // Metric modifiers
        let metric_modifiers: metrics::ModifierSet = {
            use metrics::Key as K;
            let mut set = metrics::ModifierSet::default();
            let pairs = [
                (K::CellWidth, config.adjust_cell_width),
                (K::CellHeight, config.adjust_cell_height),
                (K::CellBaseline, config.adjust_font_baseline),
                (K::UnderlinePosition, config.adjust_underline_position),
                (K::UnderlineThickness, config.adjust_underline_thickness),
                (K::StrikethroughPosition, config.adjust_strikethrough_position),
                (K::StrikethroughThickness, config.adjust_strikethrough_thickness),
                (K::OverlinePosition, config.adjust_overline_position),
                (K::OverlineThickness, config.adjust_overline_thickness),
                (K::CursorThickness, config.adjust_cursor_thickness),
                (K::CursorHeight, config.adjust_cursor_height),
                (K::BoxThickness, config.adjust_box_thickness),
                (K::IconHeight, config.adjust_icon_height),
            ];
            for (k, m) in pairs {
                if let Some(m) = m {
                    set.insert(k, m);
                }
            }
            set
        };

        let regular_offset = config.font_family.list.len();
        let bold_offset = regular_offset + config.font_family_bold.list.len();
        let italic_offset = bold_offset + config.font_family_italic.list.len();
        let bold_italic_offset = italic_offset + config.font_family_bold_italic.list.len();

        Key {
            descriptors,
            style_offsets: [regular_offset, bold_offset, italic_offset, bold_italic_offset],
            codepoint_map,
            metric_modifiers,
            font_size,
            freetype_load_flags: config.freetype_load_flags,
        }
    }

    /// Get the descriptors for the given font style that can be
    /// used with discovery.
    pub fn descriptors_for_style(&self, style: Style) -> &[discovery::Descriptor] {
        let idx = style as usize;
        let start: usize = if idx == 0 { 0 } else { self.style_offsets[idx - 1] };
        let end = self.style_offsets[idx];
        &self.descriptors[start..end]
    }

    /// Hash the key with the given hasher.
    pub fn hash<H: std::hash::Hasher>(&self, hasher: &mut H) {
        hasher.write(&self.font_size.points.to_bits().to_le_bytes());
        hasher.write(&self.font_size.xdpi.to_le_bytes());
        hasher.write(&self.font_size.ydpi.to_le_bytes());
        hasher.write(&(self.descriptors.len() as u64).to_le_bytes());
        for d in &self.descriptors {
            d.hash(hasher);
        }
        self.codepoint_map.hash(hasher);
        hasher.write(&(self.metric_modifiers.len() as u64).to_le_bytes());
        hasher.write(&[freetype_load_flags_bits(self.freetype_load_flags)]);
        if !self.metric_modifiers.is_empty() {
            for key in metrics::Key::ALL {
                if let Some(value) = self.metric_modifiers.get(&key) {
                    hasher.write(&[key as u8]);
                    value.hash(hasher);
                }
            }
        }
    }

    /// Returns a hash code that can be used to uniquely identify this
    /// action.
    pub fn hashcode(&self) -> u64 {
        let mut hasher = Wyhash::init(0);
        self.hash(&mut hasher);
        hasher.final_()
    }
}

/// The packed-struct bits of the load flags (Zig `autoHash` of the
/// `packed struct`).
fn freetype_load_flags_bits(f: FreetypeLoadFlags) -> u8 {
    (f.hinting as u8)
        | (f.force_autohint as u8) << 1
        | (f.monochrome as u8) << 2
        | (f.autohint as u8) << 3
        | (f.light as u8) << 4
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key() {
        let cfg = Config::default();

        let keycfg = DerivedConfig::init(&cfg);

        let k = Key::init(&keycfg, DesiredSize::new(12.0));

        let k2 = Key::init(&keycfg, DesiredSize::new(12.0));

        assert!(k.hashcode() > 0);
        assert_eq!(k.hashcode(), k2.hashcode());
    }

    #[test]
    fn key_different_font_points() {
        let cfg = Config::default();

        let keycfg = DerivedConfig::init(&cfg);

        let k = Key::init(&keycfg, DesiredSize::new(12.0));

        let k2 = Key::init(&keycfg, DesiredSize::new(16.0));

        assert!(k.hashcode() != k2.hashcode());
    }

    #[test]
    fn key_different_font_dpi() {
        let cfg = Config::default();

        let keycfg = DerivedConfig::init(&cfg);

        let mut s1 = DesiredSize::new(12.0);
        s1.xdpi = 1;
        let k = Key::init(&keycfg, s1);

        let mut s2 = DesiredSize::new(12.0);
        s2.xdpi = 2;
        let k2 = Key::init(&keycfg, s2);

        assert!(k.hashcode() != k2.hashcode());
    }

    #[test]
    fn shared_grid_set() {
        let mut set = SharedGridSet::init().unwrap();

        let cfg = Config::default();

        let keycfg = DerivedConfig::init(&cfg);

        // Get a grid for the given config
        let (key1, grid1) = set.r#ref(&keycfg, DesiredSize::new(12.0)).unwrap();
        assert_eq!(1usize, set.count());

        // Get another
        let (key2, grid2) = set.r#ref(&keycfg, DesiredSize::new(12.0)).unwrap();
        assert_eq!(1usize, set.count());

        // They should be pointer equivalent
        assert!(Arc::ptr_eq(&grid1, &grid2));
        drop(grid1);
        drop(grid2);

        // If I deref grid2 then we should still have a count of 1
        set.deref(&key2);
        assert_eq!(1usize, set.count());

        // If I deref grid1 then we should have a count of 0
        set.deref(&key1);
        assert_eq!(0usize, set.count());
    }
}
