//! Port of `src/macos/text/stylized_strings.zig`.
//!
//! STATUS: ported

use std::ffi::c_void;

use super::c;
use crate::pkg::macos::foundation::{self, AttributeKey};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringAttribute {
    Font,
    ParagraphStyle,
    WritingDirection,
}

impl StringAttribute {
    pub fn key(self) -> &'static foundation::String {
        // SAFETY: reading framework constants, which are CFStrings.
        unsafe {
            &*(match self {
                StringAttribute::Font => c::kCTFontAttributeName,
                StringAttribute::ParagraphStyle => c::kCTParagraphStyleAttributeName,
                StringAttribute::WritingDirection => c::kCTWritingDirectionAttributeName,
            } as *const foundation::String)
        }
    }
}

impl AttributeKey for StringAttribute {
    fn key_ptr(&self) -> *const c_void {
        self.key().as_ptr()
    }
}
