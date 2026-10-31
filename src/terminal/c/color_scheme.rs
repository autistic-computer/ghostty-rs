//! Port of `src/terminal/c/color_scheme.zig`.
//!
//! STATUS: ported
//!
//! Notes on the port: `device_status.ColorScheme` is a `lib.Enum` (`c_int`
//! in C ABI builds); the Rust core enum is `u8`-backed, so [`ColorScheme`]
//! is its C mirror and the extern function takes the raw `c_int`.

use std::ffi::c_int;

use super::result::Result;
use super::types::encode_to_buffer;
use crate::terminal::device_status;

crate::c_api_enum! {
    /// C: GhosttyColorScheme
    pub enum ColorScheme {
        Light = 0,
        Dark = 1,
    }
}

impl ColorScheme {
    pub fn to_zig(self) -> device_status::ColorScheme {
        match self {
            ColorScheme::Light => device_status::ColorScheme::Light,
            ColorScheme::Dark => device_status::ColorScheme::Dark,
        }
    }
}

/// C: ghostty_color_scheme_report_encode
///
/// # Safety
/// A non-null `out_` must be valid for `out_len` writable bytes.
#[export_name = "ghostty_color_scheme_report_encode"]
pub unsafe extern "C" fn report_encode(
    scheme: c_int,
    out_: *mut u8,
    out_len: usize,
    out_written: &mut usize,
) -> Result {
    let Some(scheme) = ColorScheme::from_int(scheme) else {
        return Result::InvalidValue;
    };
    let scheme = scheme.to_zig();
    encode_to_buffer(out_, out_len, out_written, |w| {
        device_status::encode_color_scheme_report(w, scheme)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_color_scheme_report_dark() {
        let mut buf = [0u8; device_status::MAX_COLOR_SCHEME_REPORT_ENCODE_SIZE];
        let mut written: usize = 0;
        let result =
            unsafe { report_encode(ColorScheme::Dark as c_int, buf.as_mut_ptr(), buf.len(), &mut written) };
        assert_eq!(Result::Success, result);
        assert_eq!(b"\x1B[?997;1n", &buf[..written]);
    }

    #[test]
    fn encode_color_scheme_report_light() {
        let mut buf = [0u8; device_status::MAX_COLOR_SCHEME_REPORT_ENCODE_SIZE];
        let mut written: usize = 0;
        let result =
            unsafe { report_encode(ColorScheme::Light as c_int, buf.as_mut_ptr(), buf.len(), &mut written) };
        assert_eq!(Result::Success, result);
        assert_eq!(b"\x1B[?997;2n", &buf[..written]);
    }

    #[test]
    fn encode_color_scheme_report_with_null_buffer() {
        let mut written: usize = 0;
        let result = unsafe { report_encode(ColorScheme::Dark as c_int, std::ptr::null_mut(), 0, &mut written) };
        assert_eq!(Result::OutOfSpace, result);
        assert_eq!(9, written);
    }

    #[test]
    fn encode_color_scheme_report_with_insufficient_buffer() {
        let mut buf = [0u8; 3];
        let mut written: usize = 0;
        let result =
            unsafe { report_encode(ColorScheme::Light as c_int, buf.as_mut_ptr(), buf.len(), &mut written) };
        assert_eq!(Result::OutOfSpace, result);
        assert_eq!(9, written);
    }

    #[test]
    fn encode_color_scheme_report_with_exact_buffer() {
        let mut buf = [0u8; 9];
        let mut written: usize = 0;
        let result =
            unsafe { report_encode(ColorScheme::Dark as c_int, buf.as_mut_ptr(), buf.len(), &mut written) };
        assert_eq!(Result::Success, result);
        assert_eq!(9, written);
    }
}
