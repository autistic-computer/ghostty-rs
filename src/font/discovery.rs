//! Port of `src/font/discovery.zig`.
//!
//! STATUS: ported
//!
//! The `CoreText` (macOS) and `Windows` discovery implementations are
//! gated on their targets and are written against the (not yet ported)
//! `crate::pkg::macos` bindings and `std::fs` respectively.

use super::face::Variation;
use crate::terminal::hash_map::Wyhash;

#[allow(unused_imports)]
use super::deferred_face::{self, DeferredFace};
#[allow(unused_imports)]
use super::library::Library;
#[allow(unused_imports)]
use super::Presentation;

#[allow(dead_code)]
const LOG: &str = "discovery";

/// Discover implementation for the compile options.
///
/// `.freetype` and `.web_canvas` have no discovery (Zig `void`).
#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "windows",
    target_arch = "wasm32"
)))]
pub type Discover = Fontconfig;

/// Discover implementation for the compile options.
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub type Discover = CoreText;

/// Discover implementation for the compile options.
#[cfg(target_os = "windows")]
pub type Discover = Windows;

/// Discover implementation for the compile options (no discovery).
#[cfg(target_arch = "wasm32")]
pub type Discover = ();

/// The iterator type of [`Discover`] (Zig `Discover.DiscoverIterator`).
#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "windows",
    target_arch = "wasm32"
)))]
pub type DiscoverIterator = FontconfigDiscoverIterator;

/// The iterator type of [`Discover`] (Zig `Discover.DiscoverIterator`).
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub type DiscoverIterator = CoreTextDiscoverIterator;

/// The iterator type of [`Discover`] (Zig `Discover.DiscoverIterator`).
#[cfg(target_os = "windows")]
pub type DiscoverIterator = WindowsDiscoverIterator;

/// Descriptor is used to search for fonts. The only required field
/// is "family". The rest are ignored unless they're set to a non-zero
/// value.
///
/// NOTE: the Zig struct borrows its strings and variations (memory owned
/// by the caller); here they are owned so `clone` is a plain deep copy.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Descriptor {
    /// Font family to search for. This can be a fully qualified font
    /// name such as "Fira Code", "monospace", "serif", etc. Memory is
    /// owned by the caller and should be freed when this descriptor
    /// is no longer in use. The discovery structs will never store the
    /// descriptor.
    ///
    /// On systems that use fontconfig (Linux), this can be a full
    /// fontconfig pattern, such as "Fira Code-14:bold".
    pub family: Option<String>,

    /// Specific font style to search for. This will filter the style
    /// string the font advertises. The "bold/italic" booleans later in this
    /// struct filter by the style trait the font has, not the string, so
    /// these can be used in conjunction or not.
    pub style: Option<String>,

    /// A codepoint that this font must be able to render.
    pub codepoint: u32,

    /// Font size in points that the font should support. For conversion
    /// to pixels, we will use 72 DPI for Mac and 96 DPI for everything else.
    /// (If pixel conversion is necessary, i.e. emoji fonts)
    pub size: f32,

    /// True if we want to search specifically for a font that supports
    /// specific styles.
    pub bold: bool,
    pub italic: bool,
    pub monospace: bool,

    /// Variation axes to apply to the font. This also impacts searching
    /// for fonts since fonts with the ability to set these variations
    /// will be preferred, but not guaranteed.
    pub variations: Vec<Variation>,
}

impl Descriptor {
    /// Hash the descriptor with the given hasher.
    pub fn hash<H: std::hash::Hasher>(&self, hasher: &mut H) {
        // autoHashStrat(.Deep) on an optional string hashes its bytes
        // only when present.
        if let Some(family) = &self.family {
            hasher.write(family.as_bytes());
        }
        if let Some(style) = &self.style {
            hasher.write(style.as_bytes());
        }
        hasher.write(&self.codepoint.to_le_bytes());
        hasher.write(&self.size.to_bits().to_le_bytes());
        hasher.write(&[self.bold as u8]);
        hasher.write(&[self.italic as u8]);
        hasher.write(&[self.monospace as u8]);
        hasher.write(&(self.variations.len() as u64).to_le_bytes());
        for variation in &self.variations {
            hasher.write(&variation.id.to_bits().to_le_bytes());

            // This is not correct, but we don't currently depend on the
            // hash value being different based on decimal values of variations.
            hasher.write(&(variation.value as i64).to_le_bytes());
        }
    }

    /// Returns a hash code that can be used to uniquely identify this
    /// action.
    pub fn hashcode(&self) -> u64 {
        let mut hasher = Wyhash::init(0);
        self.hash(&mut hasher);
        hasher.final_()
    }

    // NOTE: Zig `clone(alloc)` (deep copy into an arena) is `Clone::clone`.

    /// Convert to Fontconfig pattern to use for lookup. The pattern does
    /// not have defaults filled/substituted (Fontconfig thing) so callers
    /// must still do this.
    pub fn to_fc_pattern(&self) -> crate::pkg::fontconfig::Pattern {
        use crate::pkg::fontconfig::{self as fc, Property, Value};
        use crate::quirks::inline_assert as assert;
        use std::ffi::CString;

        // Zig strings are NUL-terminated; strings with interior NULs are
        // truncated at the first NUL like a C string would be.
        fn cstring(s: &str) -> CString {
            let bytes = s.as_bytes();
            let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
            CString::new(&bytes[..end]).unwrap()
        }

        let pat = fc::Pattern::create();
        if let Some(family) = &self.family {
            let family = cstring(family);
            assert(pat.add(Property::Family, Value::String(&family), false));
        }
        if let Some(style) = &self.style {
            let style = cstring(style);
            assert(pat.add(Property::Style, Value::String(&style), false));
        }
        if self.codepoint > 0 {
            let cs = fc::CharSet::create();
            assert(cs.add_char(self.codepoint));
            assert(pat.add(Property::Charset, Value::CharSet(cs), false));
            // SAFETY: fontconfig copied the charset into the pattern.
            unsafe { cs.destroy() };
        }
        if self.size > 0.0 {
            assert(pat.add(
                Property::Size,
                Value::Integer(self.size.round() as i32),
                false,
            ));
        }
        if self.bold {
            assert(pat.add(
                Property::Weight,
                Value::Integer(fc::Weight::Bold as i32),
                false,
            ));
        }
        if self.italic {
            assert(pat.add(
                Property::Slant,
                Value::Integer(fc::Slant::Italic as i32),
                false,
            ));
        }

        // For fontconfig, we always add monospace in the pattern. Since
        // fontconfig sorts by closeness to the pattern, this doesn't fully
        // exclude non-monospace but helps prefer it.
        assert(pat.add(
            Property::Spacing,
            Value::Integer(fc::Spacing::Mono as i32),
            false,
        ));

        pat
    }

    /// Convert to Core Text font descriptor to use for lookup or
    /// conversion to a specific font.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub fn to_core_text_descriptor(
        &self,
    ) -> Result<&'static crate::pkg::macos::text::FontDescriptor, crate::pkg::macos::Error> {
        use crate::pkg::macos;

        let attrs = macos::foundation::MutableDictionary::create(0)?;
        let _attrs_release = scopeguard(|| attrs.release());

        // Family
        if let Some(family_bytes) = &self.family {
            let family = macos::foundation::String::create_with_bytes(
                family_bytes.as_bytes(),
                macos::foundation::StringEncoding::Utf8,
                false,
            )?;
            // SAFETY: both are live CF objects; the dictionary retains them.
            unsafe { attrs.set_value(macos::text::FontAttribute::FamilyName.key().as_ptr(), family.as_ptr()) };
            family.release();
        }

        // Style
        if let Some(style_bytes) = &self.style {
            let style = macos::foundation::String::create_with_bytes(
                style_bytes.as_bytes(),
                macos::foundation::StringEncoding::Utf8,
                false,
            )?;
            // SAFETY: both are live CF objects; the dictionary retains them.
            unsafe { attrs.set_value(macos::text::FontAttribute::StyleName.key().as_ptr(), style.as_ptr()) };
            style.release();
        }

        // Codepoint support
        if self.codepoint > 0 {
            let cs = macos::foundation::CharacterSet::create_with_characters_in_range(
                macos::foundation::Range {
                    location: self.codepoint as _,
                    length: 1,
                },
            )?;
            // SAFETY: both are live CF objects; the dictionary retains them.
            unsafe { attrs.set_value(macos::text::FontAttribute::CharacterSet.key().as_ptr(), cs.as_ptr()) };
            cs.release();
        }

        // Set our size attribute if set
        if self.size > 0.0 {
            let size32: i32 = self.size.round() as i32;
            let size = macos::foundation::Number::create(macos::foundation::NumberType::Sint32, &size32)?;
            // SAFETY: both are live CF objects; the dictionary retains them.
            unsafe { attrs.set_value(macos::text::FontAttribute::Size.key().as_ptr(), size.as_ptr()) };
            size.release();
        }

        // Build our traits. If we set any, then we store it in the attributes
        // otherwise we do nothing. We determine this by setting up the packed
        // struct, converting to an int, and checking if it is non-zero.
        let traits = macos::text::FontSymbolicTraits {
            bold: self.bold,
            italic: self.italic,
            monospace: self.monospace,
            ..Default::default()
        };
        let traits_cval: u32 = traits.to_bits();
        if traits_cval > 0 {
            // Setting traits is a pain. We have to create a nested dictionary
            // of the symbolic traits value, and set that in our attributes.
            let traits_num = macos::foundation::Number::create(macos::foundation::NumberType::Sint32, &traits_cval)?;

            let traits_dict = macos::foundation::MutableDictionary::create(0)?;
            // SAFETY: both are live CF objects; the dictionary retains them.
            unsafe { traits_dict.set_value(macos::text::FontTraitKey::Symbolic.key().as_ptr(), traits_num.as_ptr()) };
            traits_num.release();

            // SAFETY: both are live CF objects; the dictionary retains them.
            unsafe { attrs.set_value(macos::text::FontAttribute::Traits.key().as_ptr(), traits_dict.as_ptr()) };
            traits_dict.release();
        }

        macos::text::FontDescriptor::create_with_attributes(attrs.as_dictionary())
    }
}

/// Run a closure when dropped (Zig `defer`).
#[cfg(any(target_os = "macos", target_os = "ios"))]
fn scopeguard<F: FnOnce()>(f: F) -> impl Drop {
    struct Guard<F: FnOnce()>(Option<F>);
    impl<F: FnOnce()> Drop for Guard<F> {
        fn drop(&mut self) {
            if let Some(f) = self.0.take() {
                f();
            }
        }
    }
    Guard(Some(f))
}

// ---------------------------------------------------------------------------
// Fontconfig
// ---------------------------------------------------------------------------

#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "windows",
    target_arch = "wasm32"
)))]
pub use self::fontconfig_impl::{Fontconfig, FontconfigDiscoverIterator};

#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "windows",
    target_arch = "wasm32"
)))]
mod fontconfig_impl {
    use super::*;
    use crate::pkg::fontconfig as fc;
    use crate::quirks::inline_assert as assert;

    /// Errors from [`Fontconfig::discover`].
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum DiscoverError {
        FontConfigFailed,
    }

    impl std::fmt::Display for DiscoverError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            std::fmt::Debug::fmt(self, f)
        }
    }

    impl std::error::Error for DiscoverError {}

    pub struct Fontconfig {
        pub fc_config: fc::Config,
    }

    impl Fontconfig {
        pub fn init(lib: &Library) -> Fontconfig {
            let _ = lib;
            // safe to call multiple times and concurrently
            let _ = fc::init();
            Fontconfig {
                fc_config: fc::init_load_config_and_fonts(),
            }
        }

        /// Discover fonts from a descriptor. This returns an iterator that can
        /// be used to build up the deferred fonts.
        pub fn discover(&self, desc: &Descriptor) -> Result<DiscoverIterator, DiscoverError> {
            // Build our pattern that we'll search for
            let pat = desc.to_fc_pattern();
            assert(self.fc_config.substitute_with_pat(pat, fc::MatchKind::Pattern));
            pat.default_substitute();

            // Search
            let res = self.fc_config.font_sort(pat, false, None);
            if res.result != fc::Result::Match {
                // SAFETY: not used afterwards.
                unsafe {
                    res.fs.destroy();
                    pat.destroy();
                }
                return Err(DiscoverError::FontConfigFailed);
            }

            Ok(DiscoverIterator {
                config: self.fc_config,
                pattern: pat,
                set: res.fs,
                variations: desc.variations.clone(),
                i: 0,
            })
        }

        /// The collection is unused by Fontconfig (it is generic here so
        /// that this doesn't depend on the `Collection` type).
        pub fn discover_fallback<C: ?Sized>(
            &self,
            collection: &mut C,
            desc: &Descriptor,
        ) -> Result<DiscoverIterator, DiscoverError> {
            let _ = collection;
            self.discover(desc)
        }
    }

    impl Drop for Fontconfig {
        /// Zig `deinit`.
        fn drop(&mut self) {
            // SAFETY: we own this config; it is not used afterwards.
            unsafe { self.fc_config.destroy() };
        }
    }

    pub struct DiscoverIterator {
        pub config: fc::Config,
        pub pattern: fc::Pattern,
        pub set: fc::FontSet,
        pub variations: Vec<Variation>,
        pub i: usize,
    }

    /// Zig `Fontconfig.DiscoverIterator`.
    pub type FontconfigDiscoverIterator = DiscoverIterator;

    impl DiscoverIterator {
        /// The fonts in the set (Zig `fonts` field).
        pub fn fonts(&self) -> &[fc::Pattern] {
            self.set.fonts()
        }

        #[allow(clippy::should_implement_trait)]
        pub fn next(&mut self) -> Result<Option<DeferredFace>, fc::Error> {
            if self.i >= self.fonts().len() {
                return Ok(None);
            }

            // Get the copied pattern from our fontset that has the
            // attributes configured for rendering.
            let font_pattern = self
                .config
                .font_render_prepare(self.pattern, self.fonts()[self.i])?;

            let fields = (|| -> Result<_, fc::Error> {
                let charset = match font_pattern.get(fc::Property::Charset, 0)? {
                    fc::Value::CharSet(cs) => cs,
                    _ => return Err(fc::Error::FontconfigTypeMismatch),
                };
                // NOTE: In vast majority of situations this contains a
                // `*const LangSet`, but Fontconfig falls back to a simple string
                // ("en") if the pattern fails to match any language. Bad bad bad
                // bad API design. But we must handle it gracefully here.
                let langset = font_pattern.get(fc::Property::Lang, 0)?;
                // SAFETY: the value points into `font_pattern`, which is
                // owned by (and destroyed with) the deferred face.
                let langset: fc::Value<'static> = unsafe { std::mem::transmute(langset) };
                Ok((charset, langset))
            })();
            let (charset, langset) = match fields {
                Ok(v) => v,
                Err(err) => {
                    // SAFETY: not used afterwards.
                    unsafe { font_pattern.destroy() };
                    return Err(err);
                }
            };

            // Increment after we return
            self.i += 1;

            Ok(Some(DeferredFace {
                fc: Some(deferred_face::Fontconfig {
                    pattern: font_pattern,
                    charset,
                    langset,
                    variations: self.variations.clone(),
                }),
                ..Default::default()
            }))
        }
    }

    impl Drop for DiscoverIterator {
        /// Zig `deinit`.
        fn drop(&mut self) {
            // SAFETY: we own the set and pattern; not used afterwards.
            unsafe {
                self.set.destroy();
                self.pattern.destroy();
            }
        }
    }

    pub use DiscoverError as FontconfigDiscoverError;
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "windows",
    target_arch = "wasm32"
)))]
pub use self::fontconfig_impl::FontconfigDiscoverError;

// ---------------------------------------------------------------------------
// CoreText
// ---------------------------------------------------------------------------

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use self::coretext_impl::{
    CoreText, CoreTextDiscoverIterator, DiscoverError as CoreTextDiscoverError,
};

#[cfg(any(target_os = "macos", target_os = "ios"))]
mod coretext_impl {
    use super::*;
    use crate::font::collection::Collection;
    use crate::font::opentype;
    use crate::pkg::macos;
    use crate::quirks::inline_assert as assert;

    /// Errors from CoreText discovery (the Zig inferred error sets).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum DiscoverError {
        /// A CoreFoundation/CoreText call failed.
        Macos(macos::Error),
        /// Loading the original face to search fallbacks for failed.
        GetFace(crate::font::collection::GetFaceError),
    }

    impl std::fmt::Display for DiscoverError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            std::fmt::Debug::fmt(self, f)
        }
    }

    impl std::error::Error for DiscoverError {}

    impl From<macos::Error> for DiscoverError {
        fn from(e: macos::Error) -> DiscoverError {
            DiscoverError::Macos(e)
        }
    }

    impl From<crate::font::collection::GetFaceError> for DiscoverError {
        fn from(e: crate::font::collection::GetFaceError) -> DiscoverError {
            DiscoverError::GetFace(e)
        }
    }

    type Error = DiscoverError;

    #[derive(Debug, Clone, Copy, Default)]
    pub struct CoreText {}

    impl CoreText {
        pub fn init(lib: &Library) -> CoreText {
            let _ = lib;
            // Required for the "interface" but does nothing for CoreText.
            CoreText {}
        }

        pub fn deinit(&mut self) {}

        /// Warm up the system font registry.
        ///
        /// The first CoreText query in a process initializes the system font
        /// database, which takes multiple milliseconds, while subsequent
        /// queries are microseconds.
        pub fn warmup() {
            let Ok(name) = macos::foundation::String::create_with_bytes(
                b"AppleColorEmoji",
                macos::foundation::StringEncoding::Utf8,
                false,
            ) else {
                return;
            };
            if let Ok(ct_font) = macos::text::Font::create_with_name(name, 12.0) {
                ct_font.release();
            }
            name.release();
        }

        /// Discover fonts from a descriptor. This returns an iterator that can
        /// be used to build up the deferred fonts.
        pub fn discover(&self, desc: &Descriptor) -> Result<DiscoverIterator, Error> {
            // Build our pattern that we'll search for
            let ct_desc = desc.to_core_text_descriptor()?;
            let _ct_desc_release = scopeguard(|| ct_desc.release());

            // Our descriptors have to be in an array
            let ct_desc_arr = [ct_desc];
            let desc_arr = macos::foundation::Array::create(&ct_desc_arr)?;
            let _desc_arr_release = scopeguard(|| desc_arr.release());

            // Build our collection
            let set = macos::text::FontCollection::create_with_font_descriptors(desc_arr)?;
            let _set_release = scopeguard(|| set.release());
            let list = set.create_matching_font_descriptors();
            let _list_release = scopeguard(|| list.release());

            // Sort our descriptors
            let mut zig_list = copy_matching_descriptors(list);
            sort_matching_descriptors(desc, &mut zig_list);

            Ok(DiscoverIterator {
                list: zig_list,
                variations: desc.variations.clone(),
                i: 0,
            })
        }

        /// Discover a font by its exact name (family, full, or PostScript
        /// name). This is significantly faster than `discover` because it
        /// avoids the system-wide font matching that CTFontCollection does
        /// (which takes multiple milliseconds). This should be preferred
        /// when the desired font is known exactly, e.g. system fonts such
        /// as Apple Color Emoji.
        ///
        /// Returns null if no font with this exact family name exists;
        /// CoreText fallback fonts are never returned.
        pub fn discover_exact_family(&self, family: &[u8]) -> Result<Option<DeferredFace>, Error> {
            let family_str = macos::foundation::String::create_with_bytes(
                family,
                macos::foundation::StringEncoding::Utf8,
                false,
            )?;
            let _family_release = scopeguard(|| family_str.release());

            // Create our font. We need a size to initialize it so we use size
            // 12 but we will alter the size later (same as DiscoverIterator).
            let ct_font = macos::text::Font::create_with_name(family_str, 12.0)?;

            // CTFontCreateWithName never returns null: if the requested font
            // isn't installed it returns a substitute font. Verify we got
            // the family we asked for, otherwise report not found.
            let found = {
                let actual = ct_font.copy_family_name();
                let mut buf = [0u8; 256];
                let found = match actual.cstring(&mut buf, macos::foundation::StringEncoding::Utf8)
                {
                    Some(actual_slice) => actual_slice == family,
                    None => false,
                };
                actual.release();
                found
            };
            if !found {
                ct_font.release();
                return Ok(None);
            }

            Ok(Some(DeferredFace {
                ct: Some(deferred_face::CoreText {
                    font: ct_font,
                    variations: Vec::new(),
                }),
                ..Default::default()
            }))
        }

        pub fn discover_fallback(
            &self,
            collection: &mut Collection,
            desc: &Descriptor,
        ) -> Result<DiscoverIterator, Error> {
            // If we have a codepoint within the CJK unified ideographs block
            // then we fallback to macOS to find a font that supports it because
            // there isn't a better way manually with CoreText that I can find that
            // properly takes into account system locale.
            //
            // References:
            // - http://unicode.org/charts/PDF/U4E00.pdf
            // - https://chromium.googlesource.com/chromium/src/+/main/third_party/blink/renderer/platform/fonts/LocaleInFonts.md#unified-han-ideographs
            if desc.codepoint >= 0x4E00 && desc.codepoint <= 0x9FFF {
                if let Some(han) = self.discover_codepoint(collection, desc)? {
                    // This is silly but our discover iterator needs a slice so
                    // we allocate here. This isn't a performance bottleneck but
                    // this is something we can optimize very easily...
                    return Ok(DiscoverIterator {
                        list: vec![han],
                        variations: desc.variations.clone(),
                        i: 0,
                    });
                }
            }

            let it = self.discover(desc)?;

            // If our normal discovery doesn't find anything and we have a specific
            // codepoint, then fallback to using CTFontCreateForString to find a
            // matching font CoreText wants to use. See:
            // https://github.com/ghostty-org/ghostty/issues/2499
            if it.list.is_empty() && desc.codepoint > 0 {
                if let Some(ct_desc) = self.discover_codepoint(collection, desc)? {
                    return Ok(DiscoverIterator {
                        list: vec![ct_desc],
                        variations: desc.variations.clone(),
                        i: 0,
                    });
                }
            }

            Ok(it)
        }

        /// Discover a font for a specific codepoint using the CoreText
        /// CTFontCreateForString API.
        fn discover_codepoint(
            &self,
            collection: &mut Collection,
            desc: &Descriptor,
        ) -> Result<Option<&'static macos::text::FontDescriptor>, Error> {
            if crate::font::OPTIONS.backend.has_freetype() {
                // If we have freetype, we can't use CoreText to find a font
                // that supports a specific codepoint because we need to
                // have a CoreText font to be able to do so.
                return Ok(None);
            }

            assert(desc.codepoint > 0);

            use crate::font::Style;

            // Get our original font. This is dependent on the requested style
            // from the descriptor.
            let original = 'original: {
                // In all the styles below, we try to match it but if we don't
                // we always fall back to some other option. The order matters
                // here.

                if desc.bold && desc.italic {
                    let entries = collection.faces.get(Style::BoldItalic);
                    if entries.count() > 0 {
                        break 'original collection.get_face_style(Style::BoldItalic)?;
                    }
                }

                if desc.bold {
                    let entries = collection.faces.get(Style::Bold);
                    if entries.count() > 0 {
                        break 'original collection.get_face_style(Style::Bold)?;
                    }
                }

                if desc.italic {
                    let entries = collection.faces.get(Style::Italic);
                    if entries.count() > 0 {
                        break 'original collection.get_face_style(Style::Italic)?;
                    }
                }

                collection.get_face_style(Style::Regular)?
            };

            // We need it in utf8 format
            let Some(ch) = char::from_u32(desc.codepoint) else {
                return Ok(None);
            };
            let mut buf = [0u8; 4];
            let s = ch.encode_utf8(&mut buf);

            // We need a CFString
            let str = macos::foundation::String::create_with_bytes(
                s.as_bytes(),
                macos::foundation::StringEncoding::Utf8,
                false,
            )?;
            let _str_release = scopeguard(|| str.release());

            // Get our range length for CTFontCreateForString. It looks like
            // the range uses UTF-16 codepoints and not UTF-32 codepoints.
            let range_len: usize = {
                let mut unichars = [0u16; 2];
                let pair = macos::foundation::string_get_surrogate_pair_for_long_character(
                    desc.codepoint,
                    &mut unichars,
                );
                if pair {
                    2
                } else {
                    1
                }
            };

            // Get our font
            let Some(font) = original
                .font
                .create_for_string(str, macos::foundation::Range::init(0, range_len))
            else {
                return Ok(None);
            };
            let _font_release = scopeguard(|| font.release());

            // Do not allow the last resort font to go through. This is the
            // last font used by CoreText if it can't find anything else and
            // only contains replacement characters.
            {
                let name_str = font.copy_post_script_name();
                // If the name doesn't fit in our buffer, then it can't
                // be the last resort font so we break out.
                let mut name_buf = [0u8; 64];
                let is_last_resort = matches!(
                    name_str.cstring(&mut name_buf, macos::foundation::StringEncoding::Utf8),
                    Some(name) if name == b"LastResort"
                );
                name_str.release();

                // If the name is "LastResort" then we don't want to use it.
                if is_last_resort {
                    return Ok(None);
                }
            }

            // Get the descriptor
            Ok(Some(font.copy_descriptor()))
        }
    }

    fn copy_matching_descriptors(
        list: &macos::foundation::Array,
    ) -> Vec<&'static macos::text::FontDescriptor> {
        let mut result = Vec::with_capacity(list.get_count());
        for i in 0..list.get_count() {
            // SAFETY: the matching descriptors array holds CTFontDescriptors.
            let desc: &'static macos::text::FontDescriptor =
                unsafe { list.get_value_at_index(i) };

            // We need to retain because once the list is freed it will
            // release all its members.
            desc.retain();
            result.push(desc);
        }
        result
    }

    fn sort_matching_descriptors(desc: &Descriptor, list: &mut [&'static macos::text::FontDescriptor]) {
        // Higher score is "less" (earlier)
        list.sort_unstable_by(|lhs, rhs| {
            let lhs_score = Score::score(desc, lhs);
            let rhs_score = Score::score(desc, rhs);
            rhs_score.int().cmp(&lhs_score.int())
        });
    }

    /// We represent our sorting score as a packed struct so that we
    /// can compare scores numerically but build scores symbolically.
    ///
    /// Note that packed structs store their fields from least to most
    /// significant, so the fields here are defined in increasing order
    /// of precedence.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    struct Score {
        /// Number of glyphs in the font, if two fonts have identical
        /// scores otherwise then we prefer the one with more glyphs.
        ///
        /// (Number of glyphs clamped at u16 intmax)
        glyph_count: u16,
        /// A fuzzy match on the style string, less important than
        /// an exact match, and less important than trait matches.
        fuzzy_style: u8,
        /// Whether the bold-ness of the font matches the descriptor.
        /// This is less important than italic because a font that's italic
        /// when it shouldn't be or not italic when it should be is a bigger
        /// problem (subjectively) than being the wrong weight.
        bold: bool,
        /// Whether the italic-ness of the font matches the descriptor.
        /// This is less important than an exact match on the style string
        /// because we want users to be allowed to override trait matching
        /// for the bold/italic/bold italic styles if they want.
        italic: bool,
        /// An exact (case-insensitive) match on the style string.
        exact_style: bool,
        /// Whether the font is monospace, this is more important than any of
        /// the other fields unless we're looking for a specific codepoint,
        /// in which case that is the most important thing.
        monospace: bool,
        /// If we're looking for a codepoint, whether this font has it.
        codepoint: bool,
    }

    impl Score {
        /// Zig `@bitCast` to the backing integer (u29).
        fn int(self) -> u32 {
            (self.glyph_count as u32)
                | (self.fuzzy_style as u32) << 16
                | (self.bold as u32) << 24
                | (self.italic as u32) << 25
                | (self.exact_style as u32) << 26
                | (self.monospace as u32) << 27
                | (self.codepoint as u32) << 28
        }

        fn score(desc: &Descriptor, ct_desc: &macos::text::FontDescriptor) -> Score {
            let mut this = Score::default();

            // We always load the font if we can since some things can only be
            // inspected on the font itself. Fonts that can't be loaded score
            // 0 automatically because we don't want a font we can't load.
            let Ok(font) = macos::text::Font::create_with_font_descriptor(ct_desc, 12.0) else {
                return this;
            };
            let _font_release = scopeguard(|| font.release());

            // We prefer fonts with more glyphs, all else being equal.
            this.glyph_count = u16::try_from(font.get_glyph_count()).unwrap_or(u16::MAX);

            // If we're searching for a codepoint, then we
            // prioritize fonts that have that codepoint.
            if desc.codepoint > 0 {
                // Turn UTF-32 into UTF-16 for CT API
                let mut unichars = [0u16; 2];
                let pair = macos::foundation::string_get_surrogate_pair_for_long_character(
                    desc.codepoint,
                    &mut unichars,
                );
                let len: usize = if pair { 2 } else { 1 };

                // Get our glyphs
                let mut glyphs: [macos::graphics::Glyph; 2] = [0, 0];
                this.codepoint =
                    font.get_glyphs_for_characters(&unichars[..len], &mut glyphs[..len]);
            }

            // Get our symbolic traits for the descriptor so we can
            // compare boolean attributes like bold, monospace, etc.
            let symbolic_traits: macos::text::FontSymbolicTraits = 'traits: {
                let Some(traits) = ct_desc.copy_attribute(macos::text::attr::Traits) else {
                    break 'traits Default::default();
                };
                let _traits_release = scopeguard(|| traits.release());

                let key = macos::text::FontTraitKey::Symbolic.key();
                // SAFETY: the symbolic trait value is a CFNumber.
                let Some(symbolic) =
                    (unsafe { traits.get_value::<macos::foundation::Number>(key.as_ptr()) })
                else {
                    break 'traits Default::default();
                };

                macos::text::FontSymbolicTraits::init(symbolic)
            };

            this.monospace = symbolic_traits.monospace;

            // We try to derived data from the font itself, which is generally
            // more reliable than only using the symbolic traits for this.
            let (is_bold, is_italic) = {
                // We start with initial guesses based on the symbolic traits,
                // but refine these with more information if we can get it.
                let mut is_italic = symbolic_traits.italic;
                let mut is_bold = symbolic_traits.bold;

                // Read the 'head' table out of the font data if it's available.
                if let Some(data) = font.copy_table(macos::text::FontTableTag::init(b"head")) {
                    match opentype::Head::init(data.as_slice()) {
                        Ok(head) => {
                            is_bold = is_bold || (head.mac_style & 1 == 1);
                            is_italic = is_italic || (head.mac_style & 2 == 2);
                        }
                        Err(err) => {
                            log::warn!(target: LOG, "error parsing head table: {:?}", err)
                        }
                    }
                    data.release();
                }

                // Read the 'OS/2' table out of the font data if it's available.
                if let Some(data) = font.copy_table(macos::text::FontTableTag::init(b"OS/2")) {
                    match opentype::Os2::init(data.as_slice()) {
                        Ok(os2) => {
                            is_bold = is_bold || os2.fs_selection.bold;
                            is_italic = is_italic || os2.fs_selection.italic;
                        }
                        Err(err) => {
                            log::warn!(target: LOG, "error parsing OS/2 table: {:?}", err)
                        }
                    }
                    data.release();
                }

                // Check if we have variation axes in our descriptor, if we
                // do then we can derive weight italic-ness or both from them.
                if let Some(axes) = font.copy_attribute(macos::text::attr::VariationAxes) {
                    // Copy the variation values for this instance of the font.
                    // if there are none then we just break out immediately.
                    if let Some(values) = font.copy_attribute(macos::text::attr::Variation) {
                        let mut buf = [0u8; 1024];

                        // If we see the 'ital' value then we ignore 'slnt'.
                        let mut ital_seen = false;

                        let len = axes.get_count();
                        for i in 0..len {
                            use macos::foundation::{Dictionary, Number, NumberType};
                            use macos::text::FontVariationAxisKey as Key;
                            // SAFETY: the variation axes attribute is an array
                            // of dictionaries keyed by `FontVariationAxisKey`.
                            let (cf_id, cf_name, cf_def) = unsafe {
                                let dict: &Dictionary = axes.get_value_at_index(i);
                                (
                                    dict.get_value::<Number>(Key::Identifier.key().as_ptr())
                                        .unwrap(),
                                    dict.get_value::<macos::foundation::String>(
                                        Key::Name.key().as_ptr(),
                                    )
                                    .unwrap(),
                                    dict.get_value::<Number>(Key::DefaultValue.key().as_ptr())
                                        .unwrap(),
                                )
                            };

                            let name_str: &[u8] = cf_name
                                .cstring(&mut buf, macos::foundation::StringEncoding::Utf8)
                                .unwrap_or(b"");

                            // Default value
                            let mut def: f64 = 0.0;
                            let _ = cf_def.get_value(NumberType::Double, &mut def);
                            // Value in this font
                            let mut val: f64 = def;
                            // SAFETY: the variation dictionary maps axis ids
                            // to CFNumbers.
                            if let Some(cf_val) =
                                unsafe { values.get_value::<Number>(cf_id.as_ptr()) }
                            {
                                let _ = cf_val.get_value(NumberType::Double, &mut val);
                            }

                            if name_str == b"wght" {
                                // Somewhat subjective threshold, we consider fonts
                                // bold if they have a 'wght' set greater than 600.
                                is_bold = val > 600.0;
                                continue;
                            }
                            if name_str == b"ital" {
                                is_italic = val > 0.5;
                                ital_seen = true;
                                continue;
                            }
                            if !ital_seen && name_str == b"slnt" {
                                // Arbitrary threshold of anything more than a 5
                                // degree clockwise slant is considered italic.
                                is_italic = val <= -5.0;
                                continue;
                            }
                        }
                        values.release();
                    }
                    axes.release();
                }

                (is_bold, is_italic)
            };

            this.bold = desc.bold == is_bold;
            this.italic = desc.italic == is_italic;

            // Get the style string from the font.
            let mut style_str_buf = [0u8; 128];
            let style_str: Vec<u8> = match ct_desc.copy_attribute(macos::text::attr::StyleName) {
                None => Vec::new(),
                Some(style) => {
                    let s = style
                        .cstring(&mut style_str_buf, macos::foundation::StringEncoding::Utf8)
                        .unwrap_or(b"")
                        .to_vec();
                    style.release();
                    s
                }
            };

            // The first string in this slice will be used for the exact match,
            // and for the fuzzy match, all matching substrings will increase
            // the rank.
            let desired_styles: Vec<&[u8]> = if let Some(s) = &desc.style {
                vec![s.as_bytes()]
            } else if desc.bold {
                // If we don't have an explicitly desired style name, we base
                // it on the bold and italic properties, this isn't ideal since
                // fonts may use style names other than these, but it helps in
                // some edge cases.
                if desc.italic {
                    vec![b"bold italic", b"bold", b"italic", b"oblique"]
                } else {
                    vec![b"bold", b"upright"]
                }
            } else if desc.italic {
                vec![b"italic", b"regular", b"oblique"]
            } else {
                vec![b"regular", b"upright"]
            };

            this.exact_style = style_str.eq_ignore_ascii_case(desired_styles[0]);
            // Our "fuzzy match" score is 0 if the desired style isn't present
            // in the string, otherwise we give higher priority for styles that
            // have fewer characters not in the desired_styles list.
            let mut fuzzy_style: u8 = u8::try_from(style_str.len()).unwrap();
            for s in &desired_styles {
                if find_ignore_case(&style_str, s).is_some() {
                    fuzzy_style = fuzzy_style.saturating_sub(s.len() as u8);
                }
            }
            this.fuzzy_style = u8::MAX.saturating_sub(fuzzy_style);

            this
        }
    }

    /// Zig `std.ascii.findIgnoreCase`.
    fn find_ignore_case(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        if needle.len() > haystack.len() {
            return None;
        }
        (0..=haystack.len() - needle.len())
            .find(|&i| haystack[i..i + needle.len()].eq_ignore_ascii_case(needle))
    }

    pub struct DiscoverIterator {
        pub list: Vec<&'static macos::text::FontDescriptor>,
        pub variations: Vec<Variation>,
        pub i: usize,
    }

    /// Zig `CoreText.DiscoverIterator`.
    pub type CoreTextDiscoverIterator = DiscoverIterator;

    impl DiscoverIterator {
        #[allow(clippy::should_implement_trait)]
        pub fn next(&mut self) -> Result<Option<DeferredFace>, Error> {
            if self.i >= self.list.len() {
                return Ok(None);
            }

            // Get our descriptor. We need to remove the character set
            // limitation because we may have used that to filter but we
            // don't want it anymore because it'll restrict the characters
            // available.
            let desc = {
                // We create a copy, overwriting the character set attribute.
                let attrs = macos::foundation::MutableDictionary::create(0)?;
                // SAFETY: the key is a live CFString and kCFNull is a
                // framework constant.
                unsafe {
                    attrs.set_value(
                        macos::text::FontAttribute::CharacterSet.key().as_ptr(),
                        macos::c::kCFNull,
                    )
                };

                let desc = macos::text::FontDescriptor::create_copy_with_attributes(
                    self.list[self.i],
                    attrs.as_dictionary(),
                );
                attrs.release();
                desc?
            };
            let _desc_release = scopeguard(|| desc.release());

            // Create our font. We need a size to initialize it so we use size
            // 12 but we will alter the size later.
            let font = macos::text::Font::create_with_font_descriptor(desc, 12.0)?;

            // Increment after we return
            self.i += 1;

            Ok(Some(DeferredFace {
                ct: Some(deferred_face::CoreText {
                    font,
                    variations: self.variations.clone(),
                }),
                ..Default::default()
            }))
        }
    }

    impl Drop for DiscoverIterator {
        /// Zig `deinit`.
        fn drop(&mut self) {
            for desc in &self.list {
                desc.release();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------

#[cfg(target_os = "windows")]
pub use self::windows_impl::{Windows, WindowsDiscoverIterator};

/// Windows font discovery. Enumerates font files in the system and
/// per-user font directories and matches them to a descriptor via
/// FreeType's family_name field (with a fallback to the SFNT name
/// table when family_name is missing).
///
/// No external service is used; each discover() call walks the
/// directories, opening candidate files with FreeType only as needed.
/// For typical Windows installations (~300 fonts) a name query is in
/// the tens of milliseconds. A codepoint fallback query may be
/// noticeably slower because every candidate has to be opened to
/// probe its CMap.
#[cfg(target_os = "windows")]
mod windows_impl {
    use super::*;
    use crate::font::face::{DesiredSize, Face, Options};
    use std::ffi::CString;

    #[derive(Debug, Clone)]
    pub struct Windows {
        pub lib: Library,
    }

    /// Windows discovery never fails up front (Zig `error{}`-ish).
    pub type DiscoverError = std::convert::Infallible;

    impl Windows {
        pub fn init(lib: &Library) -> Windows {
            Windows { lib: lib.clone() }
        }

        pub fn deinit(&mut self) {}

        pub fn discover(&self, desc: &Descriptor) -> Result<DiscoverIterator, DiscoverError> {
            Ok(DiscoverIterator {
                lib: self.lib.clone(),
                desc: desc.clone(),
                variations: desc.variations.clone(),
                state: State::System,
                iter: None,
                system_path: None,
                user_path: None,
            })
        }

        pub fn discover_fallback<C: ?Sized>(
            &self,
            collection: &mut C,
            desc: &Descriptor,
        ) -> Result<DiscoverIterator, DiscoverError> {
            let _ = collection;
            self.discover(desc)
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum State {
        System,
        User,
        Done,
    }

    pub struct DiscoverIterator {
        lib: Library,
        desc: Descriptor,
        variations: Vec<Variation>,
        state: State,
        /// Zig keeps both the `dir` and its iterator; `ReadDir` is both.
        iter: Option<std::fs::ReadDir>,
        system_path: Option<String>,
        user_path: Option<String>,
    }

    /// Zig `Windows.DiscoverIterator`.
    pub type WindowsDiscoverIterator = DiscoverIterator;

    impl DiscoverIterator {
        #[allow(clippy::should_implement_trait)]
        pub fn next(&mut self) -> Result<Option<DeferredFace>, crate::font::face::freetype::Error> {
            loop {
                // Ensure we have a directory iterator for the current state.
                if self.iter.is_none() {
                    match self.state {
                        State::System => {
                            let Some(path) = self.system_fonts_path() else {
                                self.state = State::User;
                                continue;
                            };
                            self.system_path = Some(path.clone());
                            match std::fs::read_dir(&path) {
                                Ok(it) => self.iter = Some(it),
                                Err(_) => {
                                    self.state = State::User;
                                    continue;
                                }
                            }
                        }
                        State::User => {
                            let Some(path) = self.user_fonts_path() else {
                                self.state = State::Done;
                                continue;
                            };
                            self.user_path = Some(path.clone());
                            match std::fs::read_dir(&path) {
                                Ok(it) => self.iter = Some(it),
                                Err(_) => {
                                    self.state = State::Done;
                                    continue;
                                }
                            }
                        }
                        State::Done => return Ok(None),
                    }
                }

                let entry = match self.iter.as_mut().unwrap().next() {
                    Some(Ok(entry)) => entry,
                    _ => {
                        // Finished this directory; advance state.
                        self.iter = None;
                        self.state = match self.state {
                            State::System => State::User,
                            State::User => State::Done,
                            State::Done => State::Done,
                        };
                        continue;
                    }
                };

                if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().into_owned();
                if !is_font_file(&name) {
                    continue;
                }

                if let Some(face) = self.try_match(&name)? {
                    return Ok(Some(face));
                }
            }
        }

        /// Build the system fonts directory from %SYSTEMROOT%. Returns null
        /// if SYSTEMROOT is unset, which shouldn't happen on a healthy
        /// Windows install but we just skip the directory rather than
        /// falling back to a hardcoded drive letter.
        fn system_fonts_path(&self) -> Option<String> {
            let systemroot = std::env::var("SYSTEMROOT").ok()?;
            Some(format!("{systemroot}\\Fonts"))
        }

        fn user_fonts_path(&self) -> Option<String> {
            let local_appdata = std::env::var("LOCALAPPDATA").ok()?;
            Some(format!("{local_appdata}\\Microsoft\\Windows\\Fonts"))
        }

        fn try_match(
            &self,
            name: &str,
        ) -> Result<Option<DeferredFace>, crate::font::face::freetype::Error> {
            let dir_path = match self.state {
                State::System => self.system_path.as_ref().unwrap(),
                State::User => self.user_path.as_ref().unwrap(),
                State::Done => return Ok(None),
            };

            let Ok(full_path) = CString::new(format!("{dir_path}\\{name}")) else {
                return Ok(None);
            };

            let is_ttc = name.len() >= 4 && name[name.len() - 4..].eq_ignore_ascii_case(".ttc");
            let max_faces: i32 = if is_ttc { 16 } else { 1 };

            // Probe each face in the file.
            for face_index in 0..max_faces {
                let Ok(face) = Face::init_file(
                    self.lib.clone(),
                    &full_path,
                    face_index,
                    Options::new(DesiredSize::new(12.0)),
                ) else {
                    break;
                };

                if self.matches(&face) {
                    return Ok(Some(self.make_deferred(face, full_path, face_index)));
                }
            }

            Ok(None)
        }

        /// Check whether the given face matches the descriptor.
        fn matches(&self, face: &Face) -> bool {
            if let Some(family) = &self.desc.family {
                if !family_matches(face, family) {
                    return false;
                }
            }
            if self.desc.codepoint != 0 && face.glyph_index(self.desc.codepoint).is_none() {
                return false;
            }
            true
        }

        fn make_deferred(&self, face: Face, full_path: CString, face_index: i32) -> DeferredFace {
            let presentation = if face.has_color() {
                Presentation::Emoji
            } else {
                Presentation::Text
            };

            DeferredFace {
                win: Some(deferred_face::Windows {
                    path: full_path,
                    face_index,
                    variations: self.variations.clone(),
                    peek: face,
                    presentation,
                }),
                ..Default::default()
            }
        }
    }

    fn is_font_file(name: &str) -> bool {
        let ends = |ext: &str| {
            name.len() >= ext.len() && name[name.len() - ext.len()..].eq_ignore_ascii_case(ext)
        };
        ends(".ttf") || ends(".ttc") || ends(".otf")
    }

    /// Compare a face's family against a requested family name. Checks
    /// FreeType's family_name first, then falls back to the SFNT name
    /// table entry.
    fn family_matches(face: &Face, family: &str) -> bool {
        // SAFETY: valid face handle; family_name is null or a C string.
        let ft_family = unsafe { (*face.face.handle).family_name };
        if !ft_family.is_null() {
            // SAFETY: non-null NUL-terminated string owned by the face.
            let f = unsafe { std::ffi::CStr::from_ptr(ft_family) };
            if f.to_bytes().eq_ignore_ascii_case(family.as_bytes()) {
                return true;
            }
        }
        let mut buf = [0u8; 256];
        let sfnt = face.name(&mut buf).unwrap_or(b"");
        !sfnt.is_empty() && sfnt.eq_ignore_ascii_case(family.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_hash() {
        let d = Descriptor::default();
        assert!(d.hashcode() != 0);
    }

    #[test]
    fn descriptor_hash_family_names() {
        let d1 = Descriptor {
            family: Some("A".into()),
            ..Default::default()
        };
        let d2 = Descriptor {
            family: Some("B".into()),
            ..Default::default()
        };
        assert!(d1.hashcode() != d2.hashcode());
    }

    #[cfg(not(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "windows",
        target_arch = "wasm32"
    )))]
    #[test]
    fn fontconfig() {
        let mut lib = Library::init().unwrap();
        {
            let fc = Fontconfig::init(&lib);
            let _it = fc
                .discover(&Descriptor {
                    family: Some("monospace".into()),
                    size: 12.0,
                    ..Default::default()
                })
                .unwrap();
        }
        lib.deinit();
    }

    #[cfg(not(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "windows",
        target_arch = "wasm32"
    )))]
    #[test]
    fn fontconfig_codepoint() {
        let mut lib = Library::init().unwrap();
        {
            let fc = Fontconfig::init(&lib);
            let mut it = fc
                .discover(&Descriptor {
                    codepoint: 'A' as u32,
                    size: 12.0,
                    ..Default::default()
                })
                .unwrap();

            // The first result should have the codepoint. Later ones may not
            // because fontconfig returns all fonts sorted.
            let face = it.next().unwrap().unwrap();
            assert!(face.has_codepoint('A' as u32, None));

            // Should have other codepoints too
            assert!(face.has_codepoint('B' as u32, None));
        }
        lib.deinit();
    }

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    #[test]
    fn coretext() {
        let mut lib = Library::init().unwrap();

        let ct = CoreText::init(&lib);
        let mut it = ct
            .discover(&Descriptor {
                family: Some("Monaco".into()),
                size: 12.0,
                ..Default::default()
            })
            .unwrap();
        let mut count: usize = 0;
        while it.next().unwrap().is_some() {
            count += 1;
        }
        assert!(count > 0);
        lib.deinit();
    }

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    #[test]
    fn coretext_codepoint() {
        let mut lib = Library::init().unwrap();

        let ct = CoreText::init(&lib);
        let mut it = ct
            .discover(&Descriptor {
                codepoint: 'A' as u32,
                size: 12.0,
                ..Default::default()
            })
            .unwrap();

        // The first result should have the codepoint. Later ones may not
        // because fontconfig returns all fonts sorted.
        let face = it.next().unwrap().unwrap();
        assert!(face.has_codepoint('A' as u32, None));

        // Should have other codepoints too
        assert!(face.has_codepoint('B' as u32, None));
        lib.deinit();
    }

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    #[test]
    #[ignore = "Disabled for now because SF Pro is not available in CI"]
    fn coretext_sorting() {
        // !!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!//
        // FIXME: Disabled for now because SF Pro is not available in CI
        //        The solution likely involves directly testing that the
        //        `sortMatchingDescriptors` function sorts a bundled test
        //        font correctly, instead of relying on the system fonts.
        // !!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!//

        let mut lib = Library::init().unwrap();
        let ct = CoreText::init(&lib);

        // We try to get a Regular, Italic, Bold, & Bold Italic version of SF Pro,
        // which should be installed on all Macs, and has many styles which makes
        // it a good test, since there will be many results for each discovery.
        let check = |bold: bool, italic: bool, expected: &[u8]| {
            let mut it = ct
                .discover(&Descriptor {
                    family: Some("SF Pro".into()),
                    size: 12.0,
                    bold,
                    italic,
                    ..Default::default()
                })
                .unwrap();
            let res = it.next().unwrap().unwrap();
            let mut buf = [0u8; 1024];
            let name = res.name(&mut buf).unwrap();
            assert_eq!(expected, name);
        };

        // Regular
        check(false, false, b"SF Pro Regular");

        // Regular Italic
        //
        // NOTE: This makes sure that we don't accidentally prefer "Thin Italic",
        //       which we previously did, because it has a shorter name.
        check(false, true, b"SF Pro Regular Italic");

        // Bold
        check(true, false, b"SF Pro Bold");

        // Bold Italic
        check(true, true, b"SF Pro Bold Italic");

        lib.deinit();
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows() {
        let mut lib = Library::init().unwrap();
        {
            let win = Windows::init(&lib);

            // Arial ships on every stock Windows install.
            let mut it = win
                .discover(&Descriptor {
                    family: Some("Arial".into()),
                    size: 12.0,
                    ..Default::default()
                })
                .unwrap();

            let face = it.next().unwrap().expect("TestFontNotFound");
            assert!(face.has_codepoint('A' as u32, None));
        }
        lib.deinit();
    }
}
