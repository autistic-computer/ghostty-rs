//! Port of `src/macos/text/typesetter.zig`.
//!
//! STATUS: ported

use super::c;
use super::line::Line;
use crate::pkg::macos::foundation;
use crate::pkg::macos::{created, opaque_type, Error};

opaque_type!(
    /// `CTTypesetterRef`
    Typesetter
);

impl Typesetter {
    pub fn create_with_attributed_string_and_options(
        str: &foundation::AttributedString,
        opts: &foundation::Dictionary,
    ) -> Result<&'static Typesetter, Error> {
        // SAFETY: both objects are live.
        unsafe {
            created(c::CTTypesetterCreateWithAttributedStringAndOptions(
                str.as_ptr(),
                opts.as_ptr(),
            ))
        }
    }

    pub fn release(&self) {
        // SAFETY: self is a live object owned by the caller.
        unsafe { c::CFRelease(self.as_ptr()) }
    }

    pub fn create_line(&self, range: c::CFRange) -> Result<&'static Line, Error> {
        // SAFETY: self is live.
        unsafe { created(c::CTTypesetterCreateLine(self.as_ptr(), range)) }
    }
}
