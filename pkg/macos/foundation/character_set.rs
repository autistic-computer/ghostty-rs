//! Port of `src/macos/foundation/character_set.zig`.
//!
//! STATUS: ported

use std::ptr;

use super::c;
use super::string::String;
use super::Range;
use crate::pkg::macos::{created, opaque_type, Error};

opaque_type!(
    /// `CFCharacterSetRef`
    CharacterSet
);

impl CharacterSet {
    pub fn create_with_characters_in_string(str: &String) -> Result<&'static CharacterSet, Error> {
        // SAFETY: str is live.
        unsafe {
            created(c::CFCharacterSetCreateWithCharactersInString(
                ptr::null(),
                str.as_ptr(),
            ))
        }
    }

    pub fn create_with_characters_in_range(range: Range) -> Result<&'static CharacterSet, Error> {
        // SAFETY: plain constructor.
        unsafe {
            created(c::CFCharacterSetCreateWithCharactersInRange(
                ptr::null(),
                range.cval(),
            ))
        }
    }

    pub fn release(&self) {
        // SAFETY: self is a live object owned by the caller.
        unsafe { c::CFRelease(self.as_ptr()) }
    }
}

#[cfg(test)]
mod tests {
    use super::super::StringEncoding;
    use super::*;

    #[test]
    fn character_set() {
        let str = String::create_with_bytes(b"hello world", StringEncoding::Ascii, false).unwrap();
        let cs = CharacterSet::create_with_characters_in_string(str).unwrap();
        cs.release();
        str.release();
    }

    #[test]
    fn character_set_range() {
        let cs = CharacterSet::create_with_characters_in_range(Range {
            location: 'A' as c::CFIndex,
            length: 1,
        })
        .unwrap();
        cs.release();
    }
}
