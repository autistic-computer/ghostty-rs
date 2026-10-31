//! Port of `src/macos/text/paragraph_style.zig`.
//!
//! STATUS: ported

use std::ffi::c_void;

use super::c;
use crate::pkg::macos::{created, opaque_type, Error};

opaque_type!(
    /// https://developer.apple.com/documentation/coretext/ctparagraphstyle?language=objc
    ParagraphStyle
);

impl ParagraphStyle {
    pub fn create(settings: &[ParagraphStyleSetting]) -> Result<&'static ParagraphStyle, Error> {
        // SAFETY: ParagraphStyleSetting is layout-compatible with
        // CTParagraphStyleSetting.
        unsafe {
            created(c::CTParagraphStyleCreate(
                settings.as_ptr() as *const c_void,
                settings.len(),
            ))
        }
    }

    pub fn release(&self) {
        // SAFETY: self is a live object owned by the caller.
        unsafe { c::CFRelease(self.as_ptr()) }
    }
}

/// https://developer.apple.com/documentation/coretext/ctparagraphstylesetting?language=objc
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ParagraphStyleSetting {
    pub spec: ParagraphStyleSpecifier,
    pub value_size: usize,
    pub value: *const c_void,
}

/// https://developer.apple.com/documentation/coretext/ctparagraphstylespecifier?language=objc
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParagraphStyleSpecifier {
    BaseWritingDirection = 13,
}

/// https://developer.apple.com/documentation/uikit/nswritingdirectionattributename?language=objc
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WritingDirection {
    Natural = -1,
    Ltr = 0,
    Rtl = 1,
    Lro = 2,
    Rlo = 3,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paragraph_style() {
        let p = ParagraphStyle::create(&[]).unwrap();
        p.release();
    }
}
