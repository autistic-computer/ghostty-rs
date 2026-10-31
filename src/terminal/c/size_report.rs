//! Port of `src/terminal/c/size_report.zig`.
//!
//! STATUS: ported

use std::ffi::c_int;

use super::result::Result;
use super::types::encode_to_buffer;
use crate::terminal::size_report as terminal_size_report;

/// C: GhosttySizeReportStyle
pub type Style = terminal_size_report::Style;

/// C: GhosttySizeReportSize
pub type Size = terminal_size_report::Size;

/// Convert a C `GhosttySizeReportStyle` value.
fn style_from_int(v: c_int) -> Option<Style> {
    [Style::Mode2048, Style::Csi14T, Style::Csi16T, Style::Csi18T, Style::Csi19T]
        .into_iter()
        .find(|s| *s as c_int == v)
}

/// C: ghostty_size_report_encode
///
/// The style is taken as the raw `c_int`; an invalid value returns
/// `invalid_value` (Rust port: Zig trusts the enum).
///
/// # Safety
/// A non-null `out_` must be valid for `out_len` writable bytes.
#[export_name = "ghostty_size_report_encode"]
pub unsafe extern "C" fn encode(
    style: c_int,
    size: Size,
    out_: *mut u8,
    out_len: usize,
    out_written: &mut usize,
) -> Result {
    let Some(style) = style_from_int(style) else {
        return Result::InvalidValue;
    };
    encode_to_buffer(out_, out_len, out_written, |w| terminal_size_report::encode(w, style, size))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: Size = Size {
        rows: 24,
        columns: 80,
        cell_width: 9,
        cell_height: 18,
    };

    #[test]
    fn encode_mode_2048() {
        let mut buf = [0u8; 64];
        let mut written: usize = 0;
        let result = unsafe { encode(Style::Mode2048 as c_int, SIZE, buf.as_mut_ptr(), buf.len(), &mut written) };
        assert_eq!(Result::Success, result);
        assert_eq!(b"\x1B[48;24;80;432;720t", &buf[..written]);
    }

    #[test]
    fn encode_csi_14_t() {
        let mut buf = [0u8; 64];
        let mut written: usize = 0;
        let result = unsafe { encode(Style::Csi14T as c_int, SIZE, buf.as_mut_ptr(), buf.len(), &mut written) };
        assert_eq!(Result::Success, result);
        assert_eq!(b"\x1b[4;432;720t", &buf[..written]);
    }

    #[test]
    fn encode_with_insufficient_buffer() {
        let mut buf = [0u8; 1];
        let mut written: usize = 0;
        let result = unsafe { encode(Style::Csi18T as c_int, SIZE, buf.as_mut_ptr(), buf.len(), &mut written) };
        assert_eq!(Result::OutOfSpace, result);
        assert!(written > 1);
    }

    #[test]
    fn encode_with_null_buffer() {
        let mut written: usize = 0;
        let result = unsafe { encode(Style::Csi18T as c_int, SIZE, std::ptr::null_mut(), 0, &mut written) };
        assert_eq!(Result::OutOfSpace, result);
        assert!(written > 0);
    }
}
