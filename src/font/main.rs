//! Port of `src/font/main.zig`.
//!
//! STATUS: ported

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "atlas.rs"]
pub mod atlas;
#[path = "backend.rs"]
pub mod backend;
#[path = "codepoint_map.rs"]
pub mod codepoint_map;
#[path = "codepoint_resolver.rs"]
pub mod codepoint_resolver;
#[path = "collection.rs"]
pub mod collection;
#[path = "deferred_face.rs"]
pub mod deferred_face;
#[path = "discovery.rs"]
pub mod discovery;
#[path = "embedded.rs"]
pub mod embedded;
#[path = "face.rs"]
pub mod face;
#[path = "glyf_rasterize.rs"]
pub mod glyf_rasterize;
#[path = "glyf_rasterize_png_test.rs"]
pub mod glyf_rasterize_png_test;
#[path = "glyph.rs"]
pub mod glyph;
#[path = "library.rs"]
pub mod library;
#[path = "metrics.rs"]
pub mod metrics;
#[path = "nerd_font_attributes.rs"]
pub mod nerd_font_attributes;
#[path = "nerd_font_tables.rs"]
pub mod nerd_font_tables;
#[path = "opentype.rs"]
pub mod opentype;
#[path = "shape.rs"]
pub mod shape;
#[path = "shaper/mod.rs"]
pub mod shaper;
#[path = "shared_grid.rs"]
pub mod shared_grid;
#[path = "shared_grid_set.rs"]
pub mod shared_grid_set;
#[path = "sprite.rs"]
pub mod sprite;
// @generated-mods-end

pub use self::atlas::Atlas;
pub use self::backend::Backend;
pub use self::codepoint_map::CodepointMap;
pub use self::codepoint_resolver::CodepointResolver;
pub use self::collection::Collection;
pub use self::deferred_face::DeferredFace;
pub use self::discovery::Descriptor;
pub use self::discovery::Discover;
pub use self::face::Face;
pub use self::glyph::Glyph;
pub use self::library::Library;
pub use self::metrics::Metrics;
pub use self::shape::Cache as ShaperCache;
pub use self::shape::Shaper;
pub use self::shared_grid::SharedGrid;
pub use self::shared_grid_set::SharedGridSet;
pub use self::sprite::Face as SpriteFace;
pub use self::sprite::Sprite;

// If we're targeting wasm then we export some wasm APIs: in Rust the
// `#[no_mangle]` exports of `deferred_face::wasm`, `face::web_canvas::wasm`
// and `shaper::web_canvas::wasm`
// are compiled on wasm32 automatically (Zig references them in a comptime
// block).

/// Build options (Zig `options`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    pub backend: Backend,
}

/// Build options. The backend is the default backend for the target
/// (the Ghostty application build).
pub const OPTIONS: Options = Options {
    backend: crate::build_config::FONT_BACKEND,
};

/// The styles that a family can take.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Style {
    Regular = 0,
    Bold = 1,
    Italic = 2,
    BoldItalic = 3,
}

impl Style {
    /// All styles in declaration order (Zig `std.enums.values(Style)`).
    pub const ALL: [Style; 4] = [Style::Regular, Style::Bold, Style::Italic, Style::BoldItalic];

    /// The Zig tag name of this style.
    pub const fn as_str(self) -> &'static str {
        match self {
            Style::Regular => "regular",
            Style::Bold => "bold",
            Style::Italic => "italic",
            Style::BoldItalic => "bold_italic",
        }
    }

    /// Inverse of `as u8` (the Zig backing integer).
    pub const fn from_int(v: u8) -> Option<Style> {
        match v {
            0 => Some(Style::Regular),
            1 => Some(Style::Bold),
            2 => Some(Style::Italic),
            3 => Some(Style::BoldItalic),
            _ => None,
        }
    }
}

impl Default for Style {
    fn default() -> Style {
        Style::Regular
    }
}

crate::cli::args::impl_cli_enum!(Style {
    Regular => "regular",
    Bold => "bold",
    Italic => "italic",
    BoldItalic => "bold_italic",
});

/// The presentation for an emoji.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Presentation {
    Text = 0,  // U+FE0E
    Emoji = 1, // U+FEOF
}

crate::cli::args::impl_cli_enum!(Presentation {
    Text => "text",
    Emoji => "emoji",
});

/// A FontIndex that can be used to use the sprite font directly.
pub const SPRITE_INDEX: collection::Index = collection::Index::init_special(collection::Special::Sprite);

/// The default font size adjustment we use when loading fallback fonts.
///
/// TODO: Add user configuration for this instead of hard-coding it.
pub const DEFAULT_FALLBACK_ADJUSTMENT: collection::SizeAdjustment = collection::SizeAdjustment::IcWidth;
