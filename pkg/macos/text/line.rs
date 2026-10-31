//! Port of `src/macos/text/line.zig`.
//!
//! STATUS: ported

use std::ptr;

use super::c;
use crate::pkg::macos::foundation;
use crate::pkg::macos::graphics;
use crate::pkg::macos::{created, opaque_type, Error};

opaque_type!(
    /// `CTLineRef`
    Line
);

impl Line {
    pub fn create_with_attributed_string(
        str: &foundation::AttributedString,
    ) -> Result<&'static Line, Error> {
        // SAFETY: str is live.
        unsafe { created(c::CTLineCreateWithAttributedString(str.as_ptr())) }
    }

    pub fn release(&self) {
        // SAFETY: self is a live object owned by the caller.
        unsafe { c::CFRelease(self.as_ptr()) }
    }

    pub fn get_glyph_count(&self) -> usize {
        // SAFETY: self is live.
        unsafe { c::CTLineGetGlyphCount(self.as_ptr()) as usize }
    }

    pub fn get_bounds_with_options(&self, opts: LineBoundsOptions) -> graphics::Rect {
        // SAFETY: self is live.
        graphics::Rect::from_c(unsafe {
            c::CTLineGetBoundsWithOptions(self.as_ptr(), opts.to_bits())
        })
    }

    pub fn get_typographic_bounds(
        &self,
        ascent: Option<&mut f64>,
        descent: Option<&mut f64>,
        leading: Option<&mut f64>,
    ) -> f64 {
        // SAFETY: self is live; the out pointers are valid or null.
        unsafe {
            c::CTLineGetTypographicBounds(
                self.as_ptr(),
                ascent.map_or(ptr::null_mut(), |p| p as *mut f64),
                descent.map_or(ptr::null_mut(), |p| p as *mut f64),
                leading.map_or(ptr::null_mut(), |p| p as *mut f64),
            )
        }
    }

    pub fn get_glyph_runs(&self) -> &foundation::Array {
        // SAFETY: self is live; the array is owned by self.
        unsafe { &*(c::CTLineGetGlyphRuns(self.as_ptr()) as *const foundation::Array) }
    }
}

/// `CTLineBoundsOptions` (a 64-bit `packed struct` in Zig).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LineBoundsOptions {
    pub exclude_leading: bool,
    pub exclude_shifts: bool,
    pub hanging_punctuation: bool,
    pub glyph_path_bounds: bool,
    pub use_optical_bounds: bool,
    pub language_extents: bool,
}

impl LineBoundsOptions {
    /// `@bitCast` to the C flags.
    pub fn to_bits(self) -> c::CTLineBoundsOptions {
        (self.exclude_leading as c::CTLineBoundsOptions)
            | (self.exclude_shifts as c::CTLineBoundsOptions) << 1
            | (self.hanging_punctuation as c::CTLineBoundsOptions) << 2
            | (self.glyph_path_bounds as c::CTLineBoundsOptions) << 3
            | (self.use_optical_bounds as c::CTLineBoundsOptions) << 4
            | (self.language_extents as c::CTLineBoundsOptions) << 5
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pkg::macos::text;

    #[test]
    fn line_bounds_options_size() {
        assert_eq!(std::mem::size_of::<c::CTLineBoundsOptions>(), 8);
    }

    #[test]
    fn line_bounds_options_bitcast() {
        let actual: c::CTLineBoundsOptions =
            c::kCTLineBoundsExcludeTypographicShifts | c::kCTLineBoundsUseOpticalBounds;
        let expected = LineBoundsOptions {
            exclude_shifts: true,
            use_optical_bounds: true,
            ..Default::default()
        };

        assert_eq!(actual, expected.to_bits());
    }

    #[test]
    fn line() {
        let font = {
            let name = foundation::String::create_with_bytes(
                b"Monaco",
                foundation::StringEncoding::Utf8,
                false,
            )
            .unwrap();
            let desc = text::FontDescriptor::create_with_name_and_size(name, 12.0).unwrap();
            let font = text::Font::create_with_font_descriptor(desc, 12.0).unwrap();
            desc.release();
            name.release();
            font
        };

        let rep = foundation::String::create_with_bytes(
            b"hello",
            foundation::StringEncoding::Utf8,
            false,
        )
        .unwrap();
        let str = foundation::MutableAttributedString::create(rep.get_length()).unwrap();
        str.replace_string(foundation::Range::init(0, 0), rep);
        str.set_attribute_value(
            foundation::Range::init(0, rep.get_length()),
            text::StringAttribute::Font,
            font,
        );

        let line = Line::create_with_attributed_string(str.as_attributed_string()).unwrap();

        assert_eq!(5usize, line.get_glyph_count());

        // TODO: this is a garbage value but should work...
        let _bounds = line.get_bounds_with_options(LineBoundsOptions::default());

        line.release();
        str.release();
        rep.release();
        font.release();
    }
}
