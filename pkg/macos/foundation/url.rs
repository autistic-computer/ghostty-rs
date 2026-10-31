//! Port of `src/macos/foundation/url.zig`.
//!
//! STATUS: ported

use std::ptr;

use super::c;
use super::string::String;
use crate::pkg::macos::{created, maybe, opaque_type, Error};

opaque_type!(
    /// `CFURLRef`
    URL
);

impl URL {
    pub fn create_with_string(str: &String, base: Option<&URL>) -> Result<&'static URL, Error> {
        // SAFETY: str (and base) are live.
        unsafe {
            created(c::CFURLCreateWithString(
                ptr::null(),
                str.as_ptr(),
                base.map_or(ptr::null(), |b| b.as_ptr()),
            ))
        }
    }

    pub fn create_with_file_system_path(
        path: &String,
        style: URLPathStyle,
        dir: bool,
    ) -> Result<&'static URL, Error> {
        // SAFETY: path is live.
        unsafe {
            created(c::CFURLCreateWithFileSystemPath(
                ptr::null(),
                path.as_ptr(),
                style as c::CFURLPathStyle,
                dir as c::Boolean,
            ))
        }
    }

    pub fn create_string_by_replacing_percent_escapes(
        str: &String,
        escape: &String,
    ) -> Result<&'static String, Error> {
        // SAFETY: both strings are live.
        unsafe {
            created(c::CFURLCreateStringByReplacingPercentEscapes(
                ptr::null(),
                str.as_ptr(),
                escape.as_ptr(),
            ))
        }
    }

    pub fn release(&self) {
        // SAFETY: self is a live object owned by the caller.
        unsafe { c::CFRelease(self.as_ptr()) }
    }

    pub fn copy_path(&self) -> Option<&'static String> {
        // SAFETY: self is live; the result is +1 or null.
        unsafe { maybe(c::CFURLCopyPath(self.as_ptr())) }
    }
}

#[repr(isize)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum URLPathStyle {
    Posix = c::kCFURLPOSIXPathStyle,
    Windows = c::kCFURLWindowsPathStyle,
}

#[cfg(test)]
mod tests {
    use super::super::StringEncoding;
    use super::*;

    #[test]
    fn url() {
        let str =
            String::create_with_bytes(b"http://www.example.com/foo", StringEncoding::Utf8, false)
                .unwrap();
        let url = URL::create_with_string(str, None).unwrap();

        {
            let path = url.copy_path().unwrap();
            let mut buf = [0u8; 128];
            let cstr = path.cstring(&mut buf, StringEncoding::Utf8).unwrap();
            assert_eq!(b"/foo", cstr);
            path.release();
        }

        url.release();
        str.release();
    }

    #[test]
    fn path() {
        let str = String::create_with_bytes(b"foo/bar.ttf", StringEncoding::Utf8, false).unwrap();
        let url = URL::create_with_file_system_path(str, URLPathStyle::Posix, false).unwrap();

        {
            let path = url.copy_path().unwrap();
            let mut buf = [0u8; 128];
            let cstr = path.cstring(&mut buf, StringEncoding::Utf8).unwrap();
            assert_eq!(b"foo/bar.ttf", cstr);
            path.release();
        }

        url.release();
        str.release();
    }
}
