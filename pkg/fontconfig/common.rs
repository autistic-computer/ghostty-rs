//! Port of `pkg/fontconfig/common.zig`.
//!
//! STATUS: ported

use std::ffi::CStr;

use super::c;
use super::error::Error;

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weight {
    Thin = c::FC_WEIGHT_THIN,
    Extralight = c::FC_WEIGHT_EXTRALIGHT,
    Light = c::FC_WEIGHT_LIGHT,
    Demilight = c::FC_WEIGHT_DEMILIGHT,
    Book = c::FC_WEIGHT_BOOK,
    Regular = c::FC_WEIGHT_REGULAR,
    Medium = c::FC_WEIGHT_MEDIUM,
    Demibold = c::FC_WEIGHT_DEMIBOLD,
    Bold = c::FC_WEIGHT_BOLD,
    Extrabold = c::FC_WEIGHT_EXTRABOLD,
    Black = c::FC_WEIGHT_BLACK,
    Extrablack = c::FC_WEIGHT_EXTRABLACK,
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slant {
    Roman = c::FC_SLANT_ROMAN,
    Italic = c::FC_SLANT_ITALIC,
    Oblique = c::FC_SLANT_OBLIQUE,
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spacing {
    Proportional = c::FC_PROPORTIONAL,
    Dual = c::FC_DUAL,
    Mono = c::FC_MONO,
    Charcell = c::FC_CHARCELL,
}

macro_rules! property_enum {
    ($($name:ident => $str:literal,)*) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Property {
            $($name,)*
        }

        impl Property {
            /// The fontconfig object name: the Zig tag name with `_`
            /// removed (e.g. `pixel_size` -> `"pixelsize"`).
            pub fn cval(self) -> &'static CStr {
                match self {
                    $(Property::$name => $str,)*
                }
            }
        }
    };
}

property_enum! {
    Family => c"family",
    Style => c"style",
    Slant => c"slant",
    Weight => c"weight",
    Size => c"size",
    Aspect => c"aspect",
    PixelSize => c"pixelsize",
    Spacing => c"spacing",
    Foundry => c"foundry",
    Antialias => c"antialias",
    Hinting => c"hinting",
    HintStyle => c"hintstyle",
    VerticalLayout => c"verticallayout",
    Autohint => c"autohint",
    GlobalAdvance => c"globaladvance",
    Width => c"width",
    File => c"file",
    Index => c"index",
    FtFace => c"ftface",
    Rasterizer => c"rasterizer",
    Outline => c"outline",
    Scalable => c"scalable",
    Color => c"color",
    Variable => c"variable",
    Scale => c"scale",
    Symbol => c"symbol",
    Dpi => c"dpi",
    Rgba => c"rgba",
    Minspace => c"minspace",
    Source => c"source",
    Charset => c"charset",
    Lang => c"lang",
    Fontversion => c"fontversion",
    Fullname => c"fullname",
    Familylang => c"familylang",
    Stylelang => c"stylelang",
    Fullnamelang => c"fullnamelang",
    Capability => c"capability",
    Embolden => c"embolden",
    EmbeddedBitmap => c"embeddedbitmap",
    Decorative => c"decorative",
    LcdFilter => c"lcdfilter",
    FontFeatures => c"fontfeatures",
    FontVariations => c"fontvariations",
    Namelang => c"namelang",
    Prgname => c"prgname",
    Hash => c"hash",
    PostscriptName => c"postscriptname",
    FontHasHint => c"fonthashint",
    Order => c"order",
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Result {
    Match = c::FcResultMatch,
    NoMatch = c::FcResultNoMatch,
    TypeMismatch = c::FcResultTypeMismatch,
    NoId = c::FcResultNoId,
    OutOfMemory = c::FcResultOutOfMemory,
}

impl Result {
    /// `@enumFromInt` for a raw `FcResult`.
    pub fn from_raw(v: c::FcResult) -> Result {
        match v {
            c::FcResultMatch => Result::Match,
            c::FcResultNoMatch => Result::NoMatch,
            c::FcResultTypeMismatch => Result::TypeMismatch,
            c::FcResultNoId => Result::NoId,
            c::FcResultOutOfMemory => Result::OutOfMemory,
            _ => unreachable!("invalid FcResult {v}"),
        }
    }

    pub fn to_error(self) -> std::result::Result<(), Error> {
        match self {
            Result::Match => Ok(()),
            Result::NoMatch => Err(Error::FontconfigNoMatch),
            Result::TypeMismatch => Err(Error::FontconfigTypeMismatch),
            Result::NoId => Err(Error::FontconfigNoId),
            Result::OutOfMemory => Err(Error::OutOfMemory),
        }
    }
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchKind {
    Pattern = c::FcMatchPattern,
    Font = c::FcMatchFont,
    Scan = c::FcMatchScan,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cval() {
        assert_eq!(c"family", Property::Family.cval());
        assert_eq!(c"pixelsize", Property::PixelSize.cval());
    }
}
