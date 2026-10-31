//! Port of `src/font/face.zig`.
//!
//! STATUS: ported

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "face/coretext.rs"]
pub mod coretext;
#[path = "face/freetype.rs"]
pub mod freetype;
#[path = "face/web_canvas.rs"]
pub mod web_canvas;
// @generated-mods-end

/// Face implementation for the compile options.
///
/// Zig switches on `options.backend`; the backends are tied to the target
/// here (see `crate::font::OPTIONS`): `freetype`, `freetype_windows`,
/// `fontconfig_freetype` and `coretext_freetype` use [`freetype::Face`],
/// the CoreText backends use `coretext::Face` and `web_canvas` uses
/// `web_canvas::Face`.
#[cfg(not(any(target_os = "macos", target_os = "ios", target_arch = "wasm32")))]
pub type Face = freetype::Face;

/// Face implementation for the compile options.
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub type Face = coretext::Face;

/// Face implementation for the compile options.
#[cfg(target_arch = "wasm32")]
pub type Face = web_canvas::Face;

const _: () = {
    // Keep the `Face` alias in sync with the configured backend.
    let has_ft = crate::font::OPTIONS.backend.has_freetype();
    let is_ft = cfg!(not(any(target_os = "macos", target_os = "ios", target_arch = "wasm32")));
    assert!(has_ft == is_ft);
};

/// If a DPI can't be calculated, this DPI is used. This is probably
/// wrong on modern devices so it is highly recommended you get the DPI
/// using whatever platform method you can.
pub const DEFAULT_DPI: u16 = if cfg!(target_os = "macos") { 72 } else { 96 };

/// These are the flags to customize how freetype loads fonts. This is
/// only non-void if the freetype backend is enabled.
///
/// NOTE: in Zig this is `void` when the backend has no FreeType; the
/// (tiny) config struct is always available here so that options can be
/// built uniformly. Only the FreeType face reads it.
pub type FreetypeLoadFlags = crate::config::FreetypeLoadFlags;

/// Zig `freetype_load_flags_default` (the config defaults, `.{}`).
pub fn freetype_load_flags_default() -> FreetypeLoadFlags {
    FreetypeLoadFlags::default()
}

/// Error returned by the faces' `name` functions (Zig `Allocator.Error`):
/// the name had to be copied into the caller's buffer and didn't fit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameError {
    OutOfMemory,
}

impl std::fmt::Display for NameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OutOfMemory")
    }
}

impl std::error::Error for NameError {}

/// Options for initializing a font face.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Options {
    pub size: DesiredSize,
    pub freetype_load_flags: FreetypeLoadFlags,
}

impl Options {
    /// Options with the given size and default values for everything
    /// else (Zig `.{ .size = size }`).
    pub fn new(size: DesiredSize) -> Options {
        Options {
            size,
            freetype_load_flags: freetype_load_flags_default(),
        }
    }
}

impl From<DesiredSize> for Options {
    fn from(size: DesiredSize) -> Options {
        Options::new(size)
    }
}

/// The desired size for loading a font.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "gtk", derive(glib::Boxed))]
#[cfg_attr(feature = "gtk", boxed_type(name = "GhosttyFontDesiredSize"))]
pub struct DesiredSize {
    // Desired size in points
    pub points: f32,

    // The DPI of the screen so we can convert points to pixels.
    pub xdpi: u16,
    pub ydpi: u16,
}

impl DesiredSize {
    /// A size in points at the default DPI (Zig `.{ .points = points }`).
    pub const fn new(points: f32) -> DesiredSize {
        DesiredSize {
            points,
            xdpi: DEFAULT_DPI,
            ydpi: DEFAULT_DPI,
        }
    }

    // Converts points to pixels
    pub fn pixels(self) -> f32 {
        // 1 point = 1/72 inch
        (self.points * self.ydpi as f32) / 72.0
    }

    /// Make this a valid gobject if we're in a GTK environment.
    #[cfg(feature = "gtk")]
    pub fn get_gobject_type() -> glib::Type {
        <DesiredSize as glib::prelude::StaticType>::static_type()
    }
}

/// A font variation setting. The best documentation for this I know of
/// is actually the CSS font-variation-settings property on MDN:
/// https://developer.mozilla.org/en-US/docs/Web/CSS/font-variation-settings
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Variation {
    pub id: VariationId,
    pub value: f64,
}

/// Zig `Variation.Id`: a `packed struct(u32)` of four bytes `d, c, b, a`
/// (so `a` is the most significant byte).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct VariationId {
    pub d: u8,
    pub c: u8,
    pub b: u8,
    pub a: u8,
}

impl VariationId {
    pub fn init(v: &[u8; 4]) -> VariationId {
        VariationId {
            a: v[0],
            b: v[1],
            c: v[2],
            d: v[3],
        }
    }

    /// Converts the ID to a string. The return value is only valid
    /// for the lifetime of the self pointer.
    pub fn str(self) -> [u8; 4] {
        [self.a, self.b, self.c, self.d]
    }

    /// The packed `u32` representation (Zig `@bitCast(id)`).
    pub fn to_bits(self) -> u32 {
        (self.d as u32) | ((self.c as u32) << 8) | ((self.b as u32) << 16) | ((self.a as u32) << 24)
    }

    /// Inverse of [`VariationId::to_bits`].
    pub fn from_bits(v: u32) -> VariationId {
        VariationId {
            d: v as u8,
            c: (v >> 8) as u8,
            b: (v >> 16) as u8,
            a: (v >> 24) as u8,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variation_id_wght_should_be_2003265652() {
        let id = VariationId::init(b"wght");
        assert_eq!(2003265652u32, id.to_bits());
        assert_eq!(b"wght", &id.str());
    }

    #[test]
    fn variation_id_slnt_should_be_1936486004() {
        let id = VariationId {
            a: b's',
            b: b'l',
            c: b'n',
            d: b't',
        };
        assert_eq!(1936486004u32, id.to_bits());
        assert_eq!(b"slnt", &id.str());
        assert_eq!(id, VariationId::from_bits(1936486004));
    }

    #[test]
    fn desired_size_pixels() {
        // Not in the Zig source; checks the default DPI and conversion.
        let size = DesiredSize::new(12.0);
        assert_eq!(DEFAULT_DPI, size.xdpi);
        assert_eq!(DEFAULT_DPI, size.ydpi);
        let size = DesiredSize {
            points: 12.0,
            xdpi: 96,
            ydpi: 96,
        };
        assert_eq!(16.0, size.pixels());
    }
}
