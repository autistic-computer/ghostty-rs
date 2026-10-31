//! Port of `src/macos/graphics/context.zig`.
//!
//! STATUS: ported
//!
//! The Zig `Context(T)` returns a namespace of functions shared by all
//! CoreGraphics context types; in Rust it is a trait with provided methods
//! implemented by each context type (e.g. `BitmapContext`).

use super::affine_transform::AffineTransform;
use super::c;
use super::geometry::Rect;
use crate::pkg::macos::CFType;

/// All the shared `CGContext` functions for a context type.
pub trait Context: CFType {
    fn release(&self) {
        // SAFETY: self is a live context owned by the caller.
        unsafe { c::CGContextRelease(self.as_ptr()) }
    }

    fn set_line_width(&self, width: f64) {
        // SAFETY: self is a live context.
        unsafe { c::CGContextSetLineWidth(self.as_ptr(), width) }
    }

    fn set_allows_antialiasing(&self, v: bool) {
        // SAFETY: self is a live context.
        unsafe { c::CGContextSetAllowsAntialiasing(self.as_ptr(), v) }
    }

    fn set_allows_font_smoothing(&self, v: bool) {
        // SAFETY: self is a live context.
        unsafe { c::CGContextSetAllowsFontSmoothing(self.as_ptr(), v) }
    }

    fn set_allows_font_subpixel_positioning(&self, v: bool) {
        // SAFETY: self is a live context.
        unsafe { c::CGContextSetAllowsFontSubpixelPositioning(self.as_ptr(), v) }
    }

    fn set_allows_font_subpixel_quantization(&self, v: bool) {
        // SAFETY: self is a live context.
        unsafe { c::CGContextSetAllowsFontSubpixelQuantization(self.as_ptr(), v) }
    }

    fn set_should_antialias(&self, v: bool) {
        // SAFETY: self is a live context.
        unsafe { c::CGContextSetShouldAntialias(self.as_ptr(), v) }
    }

    fn set_should_smooth_fonts(&self, v: bool) {
        // SAFETY: self is a live context.
        unsafe { c::CGContextSetShouldSmoothFonts(self.as_ptr(), v) }
    }

    fn set_should_subpixel_position_fonts(&self, v: bool) {
        // SAFETY: self is a live context.
        unsafe { c::CGContextSetShouldSubpixelPositionFonts(self.as_ptr(), v) }
    }

    fn set_should_subpixel_quantize_fonts(&self, v: bool) {
        // SAFETY: self is a live context.
        unsafe { c::CGContextSetShouldSubpixelQuantizeFonts(self.as_ptr(), v) }
    }

    fn set_gray_fill_color(&self, gray: f64, alpha: f64) {
        // SAFETY: self is a live context.
        unsafe { c::CGContextSetGrayFillColor(self.as_ptr(), gray, alpha) }
    }

    fn set_gray_stroke_color(&self, gray: f64, alpha: f64) {
        // SAFETY: self is a live context.
        unsafe { c::CGContextSetGrayStrokeColor(self.as_ptr(), gray, alpha) }
    }

    fn set_rgb_fill_color(&self, r: f64, g: f64, b: f64, alpha: f64) {
        // SAFETY: self is a live context.
        unsafe { c::CGContextSetRGBFillColor(self.as_ptr(), r, g, b, alpha) }
    }

    fn set_rgb_stroke_color(&self, r: f64, g: f64, b: f64, alpha: f64) {
        // SAFETY: self is a live context.
        unsafe { c::CGContextSetRGBStrokeColor(self.as_ptr(), r, g, b, alpha) }
    }

    fn set_text_drawing_mode(&self, mode: TextDrawingMode) {
        // SAFETY: self is a live context.
        unsafe { c::CGContextSetTextDrawingMode(self.as_ptr(), mode as c::CGTextDrawingMode) }
    }

    fn set_text_matrix(&self, matrix: AffineTransform) {
        // SAFETY: self is a live context.
        unsafe { c::CGContextSetTextMatrix(self.as_ptr(), matrix.cval()) }
    }

    fn set_text_position(&self, x: f64, y: f64) {
        // SAFETY: self is a live context.
        unsafe { c::CGContextSetTextPosition(self.as_ptr(), x, y) }
    }

    fn fill_rect(&self, rect: Rect) {
        // SAFETY: self is a live context.
        unsafe { c::CGContextFillRect(self.as_ptr(), rect.cval()) }
    }

    fn scale_ctm(&self, sx: c::CGFloat, sy: c::CGFloat) {
        // SAFETY: self is a live context.
        unsafe { c::CGContextScaleCTM(self.as_ptr(), sx, sy) }
    }

    fn translate_ctm(&self, tx: c::CGFloat, ty: c::CGFloat) {
        // SAFETY: self is a live context.
        unsafe { c::CGContextTranslateCTM(self.as_ptr(), tx, ty) }
    }
}

#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextDrawingMode {
    Fill = c::kCGTextFill,
    Stroke = c::kCGTextStroke,
    FillStroke = c::kCGTextFillStroke,
    Invisible = c::kCGTextInvisible,
    FillClip = c::kCGTextFillClip,
    StrokeClip = c::kCGTextStrokeClip,
    FillStrokeClip = c::kCGTextFillStrokeClip,
    Clip = c::kCGTextClip,
}
