//! Port of `src/macos/text/font_descriptor.zig`.
//!
//! STATUS: ported
//!
//! The Zig `FontAttribute.Value()` comptime function (the type of an
//! attribute's value) is modeled with the [`FontAttributeType`] trait,
//! implemented by a marker type per attribute in [`attr`], so that
//! `copy_attribute(attr::Name)` returns a typed `&String` like
//! `copyAttribute(.name)` does in Zig.

use std::ffi::c_void;

use super::c;
use crate::pkg::macos::foundation::{self, AttributeKey};
use crate::pkg::macos::{created, maybe, opaque_type, Error};

opaque_type!(
    /// `CTFontDescriptorRef`
    FontDescriptor
);

impl FontDescriptor {
    pub fn create_with_name_and_size(
        name: &foundation::String,
        size: f64,
    ) -> Result<&'static FontDescriptor, Error> {
        // SAFETY: name is live.
        unsafe {
            created(c::CTFontDescriptorCreateWithNameAndSize(
                name.as_ptr(),
                size,
            ))
        }
    }

    pub fn create_with_attributes(
        dict: &foundation::Dictionary,
    ) -> Result<&'static FontDescriptor, Error> {
        // SAFETY: dict is live.
        unsafe { created(c::CTFontDescriptorCreateWithAttributes(dict.as_ptr())) }
    }

    pub fn create_copy_with_attributes(
        original: &FontDescriptor,
        dict: &foundation::Dictionary,
    ) -> Result<&'static FontDescriptor, Error> {
        // SAFETY: both objects are live.
        unsafe {
            created(c::CTFontDescriptorCreateCopyWithAttributes(
                original.as_ptr(),
                dict.as_ptr(),
            ))
        }
    }

    pub fn create_copy_with_variation(
        original: &FontDescriptor,
        id: &foundation::Number,
        value: f64,
    ) -> Result<&'static FontDescriptor, Error> {
        // SAFETY: both objects are live.
        unsafe {
            created(c::CTFontDescriptorCreateCopyWithVariation(
                original.as_ptr(),
                id.as_ptr(),
                value,
            ))
        }
    }

    pub fn retain(&self) {
        // SAFETY: self is live.
        unsafe {
            c::CFRetain(self.as_ptr());
        }
    }

    pub fn release(&self) {
        // SAFETY: self is a live object owned by the caller.
        unsafe { c::CFRelease(self.as_ptr()) }
    }

    /// Copy an attribute value (+1 reference, or `None`).
    pub fn copy_attribute<A: FontAttributeType>(&self, attr: A) -> Option<&'static A::Value> {
        let _ = attr;
        // SAFETY: self is live; the attribute's value type is A::Value.
        unsafe {
            maybe(c::CTFontDescriptorCopyAttribute(
                self.as_ptr(),
                A::ATTR.key().as_ptr(),
            ))
        }
    }

    pub fn copy_attributes(&self) -> &'static foundation::Dictionary {
        // SAFETY: self is live; CT always returns a dictionary.
        unsafe {
            &*(c::CTFontDescriptorCopyAttributes(self.as_ptr()) as *const foundation::Dictionary)
        }
    }
}

/// An attribute marker type with its value type (`FontAttribute.Value()`).
pub trait FontAttributeType {
    const ATTR: FontAttribute;
    type Value;
}

macro_rules! font_attributes {
    ($($variant:ident, $marker:ident, $key:ident, $value:ty;)*) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum FontAttribute {
            $($variant,)*
        }

        impl FontAttribute {
            pub fn key(self) -> &'static foundation::String {
                // SAFETY: reading framework constants, which are CFStrings.
                unsafe {
                    &*(match self {
                        $(FontAttribute::$variant => c::$key,)*
                    } as *const foundation::String)
                }
            }
        }

        /// Marker types for [`FontAttribute`] values with their value types.
        pub mod attr {
            use super::*;
            $(
                #[derive(Debug, Clone, Copy)]
                pub struct $marker;
                impl FontAttributeType for $marker {
                    const ATTR: FontAttribute = FontAttribute::$variant;
                    type Value = $value;
                }
            )*
        }
    };
}

font_attributes! {
    Url, Url, kCTFontURLAttribute, foundation::URL;
    Name, Name, kCTFontNameAttribute, foundation::String;
    DisplayName, DisplayName, kCTFontDisplayNameAttribute, foundation::String;
    FamilyName, FamilyName, kCTFontFamilyNameAttribute, foundation::String;
    StyleName, StyleName, kCTFontStyleNameAttribute, foundation::String;
    Traits, Traits, kCTFontTraitsAttribute, foundation::Dictionary;
    Variation, Variation, kCTFontVariationAttribute, foundation::Dictionary;
    Size, Size, kCTFontSizeAttribute, foundation::Number;
    Matrix, Matrix, kCTFontMatrixAttribute, c_void; // CFDataRef
    CascadeList, CascadeList, kCTFontCascadeListAttribute, foundation::Array;
    CharacterSet, CharacterSet, kCTFontCharacterSetAttribute, c_void; // CFCharacterSetRef
    Languages, Languages, kCTFontLanguagesAttribute, foundation::Array;
    BaselineAdjust, BaselineAdjust, kCTFontBaselineAdjustAttribute, foundation::Number;
    MacintoshEncodings, MacintoshEncodings, kCTFontMacintoshEncodingsAttribute, foundation::Number;
    Features, Features, kCTFontFeaturesAttribute, foundation::Array;
    FeatureSettings, FeatureSettings, kCTFontFeatureSettingsAttribute, foundation::Array;
    FixedAdvance, FixedAdvance, kCTFontFixedAdvanceAttribute, foundation::Number;
    Orientation, Orientation, kCTFontOrientationAttribute, foundation::Number;
    Format, Format, kCTFontFormatAttribute, foundation::Number;
    RegistrationScope, RegistrationScope, kCTFontRegistrationScopeAttribute, foundation::Number;
    Priority, Priority, kCTFontPriorityAttribute, foundation::Number;
    Enabled, Enabled, kCTFontEnabledAttribute, foundation::Number;
    Downloadable, Downloadable, kCTFontDownloadableAttribute, c_void; // CFBoolean
    Downloaded, Downloaded, kCTFontDownloadedAttribute, c_void; // CFBoolean
    // https://developer.apple.com/documentation/coretext/core_text_constants?language=objc
    VariationAxes, VariationAxes, kCTFontVariationAxesAttribute, foundation::Array;
}

impl AttributeKey for FontAttribute {
    fn key_ptr(&self) -> *const c_void {
        self.key().as_ptr()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontTraitKey {
    Symbolic,
    Weight,
    Width,
    Slant,
}

impl FontTraitKey {
    pub fn key(self) -> &'static foundation::String {
        // SAFETY: reading framework constants, which are CFStrings.
        unsafe {
            &*(match self {
                FontTraitKey::Symbolic => c::kCTFontSymbolicTrait,
                FontTraitKey::Weight => c::kCTFontWeightTrait,
                FontTraitKey::Width => c::kCTFontWidthTrait,
                FontTraitKey::Slant => c::kCTFontSlantTrait,
            } as *const foundation::String)
        }
    }

    // `Value()`: every trait value is a `foundation::Number`.
}

// https://developer.apple.com/documentation/coretext/ctfont/font_variation_axis_dictionary_keys?language=objc
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontVariationAxisKey {
    Identifier,
    MinimumValue,
    MaximumValue,
    DefaultValue,
    Name,
    Hidden,
}

impl FontVariationAxisKey {
    pub fn key(self) -> &'static foundation::String {
        // SAFETY: reading framework constants, which are CFStrings.
        unsafe {
            &*(match self {
                FontVariationAxisKey::Identifier => c::kCTFontVariationAxisIdentifierKey,
                FontVariationAxisKey::MinimumValue => c::kCTFontVariationAxisMinimumValueKey,
                FontVariationAxisKey::MaximumValue => c::kCTFontVariationAxisMaximumValueKey,
                FontVariationAxisKey::DefaultValue => c::kCTFontVariationAxisDefaultValueKey,
                FontVariationAxisKey::Name => c::kCTFontVariationAxisNameKey,
                FontVariationAxisKey::Hidden => c::kCTFontVariationAxisHiddenKey,
            } as *const foundation::String)
        }
    }

    // `Value()`: `name` is a `foundation::String`, everything else a
    // `foundation::Number`.
}

/// `CTFontSymbolicTraits` (a `packed struct(u32)` in Zig).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FontSymbolicTraits {
    pub italic: bool,
    pub bold: bool,
    pub expanded: bool,
    pub condensed: bool,
    pub monospace: bool,
    pub vertical: bool,
    pub ui_optimized: bool,
    pub color_glyphs: bool,
    pub composite: bool,
}

impl FontSymbolicTraits {
    pub fn init(num: &foundation::Number) -> FontSymbolicTraits {
        let mut raw: i32 = 0;
        let _ = num.get_value(foundation::NumberType::Sint32, &mut raw);
        FontSymbolicTraits::from_bits(raw as u32)
    }

    /// `@bitCast` from the C value.
    pub fn from_bits(v: c::CTFontSymbolicTraits) -> FontSymbolicTraits {
        let bit = |n: u32| v & (1 << n) != 0;
        FontSymbolicTraits {
            italic: bit(0),
            bold: bit(1),
            expanded: bit(5),
            condensed: bit(6),
            monospace: bit(10),
            vertical: bit(11),
            ui_optimized: bit(12),
            color_glyphs: bit(13),
            composite: bit(14),
        }
    }

    /// `@bitCast` to the C value.
    pub fn to_bits(self) -> c::CTFontSymbolicTraits {
        (self.italic as u32)
            | (self.bold as u32) << 1
            | (self.expanded as u32) << 5
            | (self.condensed as u32) << 6
            | (self.monospace as u32) << 10
            | (self.vertical as u32) << 11
            | (self.ui_optimized as u32) << 12
            | (self.color_glyphs as u32) << 13
            | (self.composite as u32) << 14
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbolic_traits_bitcast() {
        let actual: c::CTFontSymbolicTraits = c::kCTFontTraitMonoSpace | c::kCTFontTraitExpanded;
        let expected = FontSymbolicTraits {
            monospace: true,
            expanded: true,
            ..Default::default()
        };

        assert_eq!(actual, expected.to_bits());
        assert_eq!(FontSymbolicTraits::from_bits(actual), expected);
    }

    #[test]
    fn symbolic_traits_number() {
        let raw: i32 = (c::kCTFontTraitMonoSpace | c::kCTFontTraitExpanded) as i32;
        let num = foundation::Number::create(foundation::NumberType::Sint32, &raw).unwrap();

        let expected = FontSymbolicTraits {
            monospace: true,
            expanded: true,
            ..Default::default()
        };
        let actual = FontSymbolicTraits::init(num);
        assert_eq!(expected, actual);
        num.release();
    }

    #[test]
    fn descriptor() {
        let name =
            foundation::String::create_with_bytes(b"foo", foundation::StringEncoding::Utf8, false)
                .unwrap();

        let v = FontDescriptor::create_with_name_and_size(name, 12.0).unwrap();

        let copy_name = v.copy_attribute(attr::Name).unwrap();

        {
            let mut buf = [0u8; 128];
            let cstr = copy_name
                .cstring(&mut buf, foundation::StringEncoding::Utf8)
                .unwrap();
            assert_eq!(b"foo", cstr);
        }

        copy_name.release();
        v.release();
        name.release();
    }
}
