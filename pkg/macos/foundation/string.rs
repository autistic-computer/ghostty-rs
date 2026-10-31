//! Port of `src/macos/foundation/string.zig`.
//!
//! STATUS: ported

use std::ffi::CStr;
use std::ptr;

use super::base::ComparisonResult;
use super::c;
use crate::pkg::macos::{created, opaque_type, Error};

opaque_type!(
    /// `CFStringRef`
    String
);

impl String {
    pub fn create_with_bytes(
        bs: &[u8],
        encoding: StringEncoding,
        external: bool,
    ) -> Result<&'static String, Error> {
        // SAFETY: bs is a valid slice.
        unsafe {
            created(c::CFStringCreateWithBytes(
                ptr::null(),
                bs.as_ptr(),
                bs.len() as c::CFIndex,
                encoding as c::CFStringEncoding,
                external as c::Boolean,
            ))
        }
    }

    /// Wrap `unichars` without copying; they must outlive the string.
    pub fn create_with_characters_no_copy(unichars: &[u16]) -> Result<&'static String, Error> {
        // SAFETY: unichars is a valid slice; kCFAllocatorNull means CF won't
        // free it.
        unsafe {
            created(c::CFStringCreateWithCharactersNoCopy(
                ptr::null(),
                unichars.as_ptr(),
                unichars.len() as c::CFIndex,
                c::kCFAllocatorNull,
            ))
        }
    }

    pub fn release(&self) {
        // SAFETY: self is a live object owned by the caller.
        unsafe { c::CFRelease(self.as_ptr()) }
    }

    pub fn get_length(&self) -> usize {
        // SAFETY: self is live.
        unsafe { c::CFStringGetLength(self.as_ptr()) as usize }
    }

    pub fn has_prefix(&self, prefix: &String) -> bool {
        // SAFETY: both strings are live.
        unsafe { c::CFStringHasPrefix(self.as_ptr(), prefix.as_ptr()) == 1 }
    }

    pub fn compare(&self, other: &String, options: StringComparison) -> ComparisonResult {
        // SAFETY: both strings are live.
        ComparisonResult::from_int(unsafe {
            c::CFStringCompare(
                self.as_ptr(),
                other.as_ptr(),
                options.to_bits() as c::CFStringCompareFlags,
            )
        })
    }

    /// Copy the string into `buf` as a C string, returning the bytes before
    /// the NUL terminator, or `None` if it doesn't fit or can't be converted.
    pub fn cstring<'a>(&self, buf: &'a mut [u8], encoding: StringEncoding) -> Option<&'a [u8]> {
        // SAFETY: buf is valid for buf.len() bytes.
        if unsafe {
            c::CFStringGetCString(
                self.as_ptr(),
                buf.as_mut_ptr().cast(),
                buf.len() as c::CFIndex,
                encoding as c::CFStringEncoding,
            )
        } == 0
        {
            return None;
        }
        let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        Some(&buf[..len])
    }

    /// The internal C string pointer, if the string has one in this
    /// encoding.
    pub fn cstring_ptr(&self, encoding: StringEncoding) -> Option<&CStr> {
        // SAFETY: self is live.
        let ptr =
            unsafe { c::CFStringGetCStringPtr(self.as_ptr(), encoding as c::CFStringEncoding) };
        if ptr.is_null() {
            return None;
        }
        // SAFETY: CF returns a NUL-terminated string valid while self lives.
        Some(unsafe { CStr::from_ptr(ptr) })
    }
}

opaque_type!(
    /// `CFMutableStringRef`
    MutableString
);

impl MutableString {
    pub fn create(cap: usize) -> Result<&'static MutableString, Error> {
        // SAFETY: plain constructor.
        unsafe { created(c::CFStringCreateMutable(ptr::null(), cap as c::CFIndex)) }
    }

    pub fn release(&self) {
        // SAFETY: self is a live object owned by the caller.
        unsafe { c::CFRelease(self.as_ptr()) }
    }

    pub fn string(&self) -> &String {
        // SAFETY: a mutable string is a string.
        unsafe { &*(self as *const Self as *const String) }
    }

    pub fn append_characters(&self, chars: &[u16]) {
        // SAFETY: self is live; chars is a valid slice.
        unsafe {
            c::CFStringAppendCharacters(self.as_ptr(), chars.as_ptr(), chars.len() as c::CFIndex)
        }
    }
}

/// `CFStringCompareFlags` (a `packed struct` the size of a `c_int` in Zig).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StringComparison {
    pub case_insensitive: bool,
    pub backwards: bool,
    pub anchored: bool,
    pub nonliteral: bool,
    pub localized: bool,
    pub numerically: bool,
    pub diacritic_insensitive: bool,
    pub width_insensitive: bool,
    pub forced_ordering: bool,
}

impl StringComparison {
    /// `@bitCast` to the C flags.
    pub fn to_bits(self) -> u32 {
        (self.case_insensitive as u32)
            | (self.backwards as u32) << 2
            | (self.anchored as u32) << 3
            | (self.nonliteral as u32) << 4
            | (self.localized as u32) << 5
            | (self.numerically as u32) << 6
            | (self.diacritic_insensitive as u32) << 7
            | (self.width_insensitive as u32) << 8
            | (self.forced_ordering as u32) << 9
    }
}

/// https://developer.apple.com/documentation/corefoundation/cfstringencoding?language=objc
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringEncoding {
    Invalid = 0xffffffff,
    MacRoman = 0,
    WindowsLatin1 = 0x0500,
    IsoLatin1 = 0x0201,
    NextstepLatin = 0x0B01,
    Ascii = 0x0600,
    Unicode = 0x0100,
    Utf8 = 0x08000100,
    NonLossyAscii = 0x0BFF,
    Utf16Be = 0x10000100,
    Utf16Le = 0x14000100,
    Utf32 = 0x0c000100,
    Utf32Be = 0x18000100,
    Utf32Le = 0x1c000100,
}

pub fn string_get_surrogate_pair_for_long_character(ch: u32, surrogates: &mut [u16]) -> bool {
    assert!(surrogates.len() >= 2);
    // SAFETY: surrogates has room for two UniChars.
    unsafe { c::CFStringGetSurrogatePairForLongCharacter(ch, surrogates.as_mut_ptr()) == 1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_comparison_bits() {
        let all = StringComparison {
            case_insensitive: true,
            backwards: true,
            anchored: true,
            nonliteral: true,
            localized: true,
            numerically: true,
            diacritic_insensitive: true,
            width_insensitive: true,
            forced_ordering: true,
        };
        assert_eq!(all.to_bits(), 0b11_1111_1101);
    }

    #[test]
    fn string() {
        let str = String::create_with_bytes(b"hello world", StringEncoding::Ascii, false).unwrap();
        let prefix = String::create_with_bytes(b"hello", StringEncoding::Ascii, false).unwrap();

        assert!(str.has_prefix(prefix));
        assert_eq!(
            ComparisonResult::Equal,
            str.compare(str, StringComparison::default())
        );
        assert_eq!(
            b"hello world",
            str.cstring_ptr(StringEncoding::Ascii).unwrap().to_bytes()
        );

        {
            let mut buf = [0u8; 128];
            let cstr = str.cstring(&mut buf, StringEncoding::Ascii).unwrap();
            assert_eq!(b"hello world", cstr);
        }

        prefix.release();
        str.release();
    }

    #[test]
    fn unichar() {
        let mut unichars = [0u16; 2];
        assert!(!string_get_surrogate_pair_for_long_character(
            'A' as u32,
            &mut unichars
        ));
    }
}
