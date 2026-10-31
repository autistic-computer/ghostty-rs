//! Port of `src/macos/text/font.zig`.
//!
//! STATUS: ported

use std::ptr;

use super::c;
use super::font_descriptor::{FontAttributeType, FontDescriptor, FontSymbolicTraits};
use crate::pkg::macos::foundation;
use crate::pkg::macos::graphics;
use crate::pkg::macos::{created, maybe, opaque_type, CFType, Error};

opaque_type!(
    /// `CTFontRef`
    Font
);

impl Font {
    pub fn create_with_font_descriptor(
        desc: &FontDescriptor,
        size: f32,
    ) -> Result<&'static Font, Error> {
        // SAFETY: desc is live.
        unsafe {
            created(c::CTFontCreateWithFontDescriptor(
                desc.as_ptr(),
                size as f64,
                ptr::null(),
            ))
        }
    }

    pub fn create_with_name(name: &foundation::String, size: f32) -> Result<&'static Font, Error> {
        // SAFETY: name is live.
        unsafe {
            created(c::CTFontCreateWithName(
                name.as_ptr(),
                size as f64,
                ptr::null(),
            ))
        }
    }

    pub fn create_for_string(
        &self,
        str: &foundation::String,
        range: foundation::Range,
    ) -> Option<&'static Font> {
        // SAFETY: self and str are live.
        unsafe {
            maybe(c::CTFontCreateForString(
                self.as_ptr(),
                str.as_ptr(),
                range.cval(),
            ))
        }
    }

    pub fn copy_with_attributes(
        &self,
        size: f32,
        matrix: Option<&graphics::AffineTransform>,
        attrs: Option<&FontDescriptor>,
    ) -> Result<&'static Font, Error> {
        // SAFETY: self (and attrs) are live; AffineTransform is
        // layout-compatible with CGAffineTransform.
        unsafe {
            created(c::CTFontCreateCopyWithAttributes(
                self.as_ptr(),
                size as f64,
                matrix.map_or(ptr::null(), |m| {
                    m as *const graphics::AffineTransform as *const c::CGAffineTransform
                }),
                attrs.map_or(ptr::null(), |a| a.as_ptr()),
            ))
        }
    }

    pub fn release(&self) {
        // SAFETY: self is a live object owned by the caller.
        unsafe { c::CFRelease(self.as_ptr()) }
    }

    pub fn retain(&self) {
        // SAFETY: self is live.
        unsafe {
            c::CFRetain(self.as_ptr());
        }
    }

    pub fn copy_descriptor(&self) -> &'static FontDescriptor {
        // SAFETY: self is live; CT always returns a descriptor.
        unsafe { &*(c::CTFontCopyFontDescriptor(self.as_ptr()) as *const FontDescriptor) }
    }

    pub fn copy_features(&self) -> &'static foundation::Array {
        // SAFETY: self is live; CT always returns an array.
        unsafe { &*(c::CTFontCopyFeatures(self.as_ptr()) as *const foundation::Array) }
    }

    pub fn copy_default_cascade_list_for_languages(&self) -> &'static foundation::Array {
        // SAFETY: self is live; CT always returns an array.
        unsafe {
            &*(c::CTFontCopyDefaultCascadeListForLanguages(self.as_ptr(), ptr::null())
                as *const foundation::Array)
        }
    }

    pub fn copy_table(&self, tag: FontTableTag) -> Option<&'static foundation::Data> {
        // SAFETY: self is live.
        unsafe {
            maybe(c::CTFontCopyTable(
                self.as_ptr(),
                tag.0,
                c::kCTFontTableOptionNoOptions,
            ))
        }
    }

    pub fn get_glyph_count(&self) -> usize {
        // SAFETY: self is live.
        unsafe { c::CTFontGetGlyphCount(self.as_ptr()) as usize }
    }

    pub fn get_glyphs_for_characters(&self, chars: &[u16], glyphs: &mut [graphics::Glyph]) -> bool {
        assert!(chars.len() == glyphs.len());
        // SAFETY: both buffers hold chars.len() elements.
        unsafe {
            c::CTFontGetGlyphsForCharacters(
                self.as_ptr(),
                chars.as_ptr(),
                glyphs.as_mut_ptr(),
                chars.len() as c::CFIndex,
            )
        }
    }

    pub fn create_path_for_glyph(&self, glyph: graphics::Glyph) -> Option<&'static graphics::Path> {
        // SAFETY: self is live.
        unsafe {
            maybe(c::CTFontCreatePathForGlyph(
                self.as_ptr(),
                glyph,
                ptr::null(),
            ))
        }
    }

    /// Draw glyphs into `context` (must be some context type from
    /// graphics).
    pub fn draw_glyphs(
        &self,
        glyphs: &[graphics::Glyph],
        positions: &[graphics::Point],
        context: &impl CFType,
    ) {
        assert!(positions.len() == glyphs.len());
        // SAFETY: both buffers hold glyphs.len() elements; Point is
        // layout-compatible with CGPoint.
        unsafe {
            c::CTFontDrawGlyphs(
                self.as_ptr(),
                glyphs.as_ptr(),
                positions.as_ptr() as *const c::CGPoint,
                glyphs.len(),
                context.as_ptr(),
            )
        }
    }

    pub fn get_bounding_rects_for_glyphs(
        &self,
        orientation: FontOrientation,
        glyphs: &[graphics::Glyph],
        rects: Option<&mut [graphics::Rect]>,
    ) -> graphics::Rect {
        if let Some(s) = &rects {
            assert!(glyphs.len() == s.len());
        }
        // SAFETY: the buffers hold glyphs.len() elements; Rect is
        // layout-compatible with CGRect.
        graphics::Rect::from_c(unsafe {
            c::CTFontGetBoundingRectsForGlyphs(
                self.as_ptr(),
                orientation as c::CTFontOrientation,
                glyphs.as_ptr(),
                rects.map_or(ptr::null_mut(), |s| s.as_mut_ptr() as *mut c::CGRect),
                glyphs.len() as c::CFIndex,
            )
        })
    }

    pub fn get_advances_for_glyphs(
        &self,
        orientation: FontOrientation,
        glyphs: &[graphics::Glyph],
        advances: Option<&mut [graphics::Size]>,
    ) -> f64 {
        if let Some(s) = &advances {
            assert!(glyphs.len() == s.len());
        }
        // SAFETY: the buffers hold glyphs.len() elements; Size is
        // layout-compatible with CGSize.
        unsafe {
            c::CTFontGetAdvancesForGlyphs(
                self.as_ptr(),
                orientation as c::CTFontOrientation,
                glyphs.as_ptr(),
                advances.map_or(ptr::null_mut(), |s| s.as_mut_ptr() as *mut c::CGSize),
                glyphs.len() as c::CFIndex,
            )
        }
    }

    /// Copy an attribute value (+1 reference, or `None`).
    pub fn copy_attribute<A: FontAttributeType>(&self, attr: A) -> Option<&'static A::Value> {
        let _ = attr;
        // SAFETY: self is live; the attribute's value type is A::Value.
        unsafe {
            maybe(c::CTFontCopyAttribute(
                self.as_ptr(),
                A::ATTR.key().as_ptr(),
            ))
        }
    }

    pub fn copy_family_name(&self) -> &'static foundation::String {
        // SAFETY: self is live; CT always returns a string.
        unsafe { &*(c::CTFontCopyFamilyName(self.as_ptr()) as *const foundation::String) }
    }

    pub fn copy_display_name(&self) -> Option<&'static foundation::String> {
        // SAFETY: self is live.
        unsafe { maybe(c::CTFontCopyDisplayName(self.as_ptr())) }
    }

    pub fn copy_post_script_name(&self) -> &'static foundation::String {
        // SAFETY: self is live; CT always returns a string.
        unsafe { &*(c::CTFontCopyPostScriptName(self.as_ptr()) as *const foundation::String) }
    }

    pub fn get_symbolic_traits(&self) -> FontSymbolicTraits {
        // SAFETY: self is live.
        FontSymbolicTraits::from_bits(unsafe { c::CTFontGetSymbolicTraits(self.as_ptr()) })
    }

    pub fn get_ascent(&self) -> f64 {
        // SAFETY: self is live.
        unsafe { c::CTFontGetAscent(self.as_ptr()) }
    }

    pub fn get_descent(&self) -> f64 {
        // SAFETY: self is live.
        unsafe { c::CTFontGetDescent(self.as_ptr()) }
    }

    pub fn get_leading(&self) -> f64 {
        // SAFETY: self is live.
        unsafe { c::CTFontGetLeading(self.as_ptr()) }
    }

    pub fn get_bounding_box(&self) -> graphics::Rect {
        // SAFETY: self is live.
        graphics::Rect::from_c(unsafe { c::CTFontGetBoundingBox(self.as_ptr()) })
    }

    pub fn get_underline_position(&self) -> f64 {
        // SAFETY: self is live.
        unsafe { c::CTFontGetUnderlinePosition(self.as_ptr()) }
    }

    pub fn get_underline_thickness(&self) -> f64 {
        // SAFETY: self is live.
        unsafe { c::CTFontGetUnderlineThickness(self.as_ptr()) }
    }

    pub fn get_cap_height(&self) -> f64 {
        // SAFETY: self is live.
        unsafe { c::CTFontGetCapHeight(self.as_ptr()) }
    }

    pub fn get_x_height(&self) -> f64 {
        // SAFETY: self is live.
        unsafe { c::CTFontGetXHeight(self.as_ptr()) }
    }

    pub fn get_units_per_em(&self) -> u32 {
        // SAFETY: self is live.
        unsafe { c::CTFontGetUnitsPerEm(self.as_ptr()) }
    }

    pub fn get_size(&self) -> f64 {
        // SAFETY: self is live.
        unsafe { c::CTFontGetSize(self.as_ptr()) }
    }
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontOrientation {
    Default = c::kCTFontOrientationDefault,
    Horizontal = c::kCTFontOrientationHorizontal,
    Vertical = c::kCTFontOrientationVertical,
}

/// `CTFontTableTag` (a non-exhaustive enum in Zig).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FontTableTag(pub u32);

impl FontTableTag {
    pub const SVG: FontTableTag = FontTableTag(c::kCTFontTableSVG);
    pub const OS2: FontTableTag = FontTableTag(c::kCTFontTableOS2);
    pub const HEAD: FontTableTag = FontTableTag(c::kCTFontTableHead);
    pub const HHEA: FontTableTag = FontTableTag(c::kCTFontTableHhea);
    pub const POST: FontTableTag = FontTableTag(c::kCTFontTablePost);

    pub fn init(v: &[u8; 4]) -> FontTableTag {
        FontTableTag(foundation::FourCharCode::init(v).to_bits())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pkg::macos::graphics::Context;

    fn monaco() -> (
        &'static foundation::String,
        &'static FontDescriptor,
        &'static Font,
    ) {
        let name = foundation::String::create_with_bytes(
            b"Monaco",
            foundation::StringEncoding::Utf8,
            false,
        )
        .unwrap();
        let desc = FontDescriptor::create_with_name_and_size(name, 12.0).unwrap();
        let font = Font::create_with_font_descriptor(desc, 12.0).unwrap();
        (name, desc, font)
    }

    #[test]
    fn table_tag() {
        assert_eq!(FontTableTag::init(b"head"), FontTableTag::HEAD);
        assert_eq!(FontTableTag::init(b"OS/2"), FontTableTag::OS2);
    }

    #[test]
    fn font() {
        let (name, desc, font) = monaco();

        // Traits
        {
            let traits = font.get_symbolic_traits();
            assert!(!traits.color_glyphs);
        }

        let mut glyphs = [0 as graphics::Glyph; 1];
        assert!(font.get_glyphs_for_characters(&['A' as u16], &mut glyphs));
        assert!(glyphs[0] > 0);

        // Bounding rect
        {
            let mut rect =
                font.get_bounding_rects_for_glyphs(FontOrientation::Horizontal, &glyphs, None);
            assert!(rect.size.width > 0.0);

            let mut singles = [graphics::Rect::default(); 1];
            rect = font.get_bounding_rects_for_glyphs(
                FontOrientation::Horizontal,
                &glyphs,
                Some(&mut singles),
            );
            assert!(rect.size.width > 0.0);
            assert!(singles[0].size.width > 0.0);
        }

        // Advances
        {
            let mut advance =
                font.get_advances_for_glyphs(FontOrientation::Horizontal, &glyphs, None);
            assert!(advance > 0.0);

            let mut singles = [graphics::Size::default(); 1];
            advance = font.get_advances_for_glyphs(
                FontOrientation::Horizontal,
                &glyphs,
                Some(&mut singles),
            );
            assert!(advance > 0.0);
            assert!(singles[0].width > 0.0);
        }

        // Draw
        {
            let cs = graphics::ColorSpace::create_device_gray().unwrap();
            let ctx = graphics::BitmapContext::create(None, 80, 80, 8, 80, cs, 0).unwrap();

            let pos = [graphics::Point { x: 0.0, y: 0.0 }];
            font.draw_glyphs(&glyphs, &pos, ctx);
            Context::release(ctx);
            cs.release();
        }

        font.release();
        desc.release();
        name.release();
    }

    #[test]
    fn copy() {
        let (name, desc, font) = monaco();
        let f2 = font.copy_with_attributes(14.0, None, None).unwrap();
        f2.release();
        font.release();
        desc.release();
        name.release();
    }
}
