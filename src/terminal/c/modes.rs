//! Port of `src/terminal/c/modes.zig`.
//!
//! STATUS: ported
//!
//! Notes on the port: Zig declares `ReportState` as a non-exhaustive
//! `enum(c_int) { _ }` converted with `toZig`; the Rust C mirror lists the
//! `modes.Report.State` keys (so it can be described in the manifest) and
//! the extern function takes the raw `c_int`.

use std::ffi::c_int;

use super::result::Result;
use super::types::encode_to_buffer;
use crate::terminal::modes;

crate::c_api_enum! {
    /// C: GhosttyModeReportState
    pub enum ReportState {
        NotRecognized = 0,
        Set = 1,
        Reset = 2,
        PermanentlySet = 3,
        PermanentlyReset = 4,
    }
}

impl ReportState {
    fn to_zig(self) -> modes::ReportState {
        match self {
            ReportState::NotRecognized => modes::ReportState::NotRecognized,
            ReportState::Set => modes::ReportState::Set,
            ReportState::Reset => modes::ReportState::Reset,
            ReportState::PermanentlySet => modes::ReportState::PermanentlySet,
            ReportState::PermanentlyReset => modes::ReportState::PermanentlyReset,
        }
    }
}

/// C: ghostty_mode_report_encode
///
/// # Safety
/// A non-null `out_` must be valid for `out_len` writable bytes.
#[export_name = "ghostty_mode_report_encode"]
pub unsafe extern "C" fn report_encode(
    tag: modes::ModeTagBacking,
    state: c_int,
    out_: *mut u8,
    out_len: usize,
    out_written: &mut usize,
) -> Result {
    let mode_tag = modes::ModeTag::from_bits(tag);
    let Some(state) = ReportState::from_int(state) else {
        return Result::InvalidValue;
    };
    let report = modes::Report {
        tag: modes::ReportTag {
            value: mode_tag.value,
            ansi: mode_tag.ansi,
        },
        state: state.to_zig(),
    };

    encode_to_buffer(out_, out_len, out_written, |w| report.encode(w))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(value: u16, ansi: bool) -> modes::ModeTagBacking {
        modes::ModeTag::new(value, ansi).to_bits()
    }

    #[test]
    fn encode_dec_mode_set() {
        let mut buf = [0u8; modes::Report::MAX_SIZE];
        let mut written: usize = 0;
        let result = unsafe { report_encode(tag(1, false), 1, buf.as_mut_ptr(), buf.len(), &mut written) };
        assert_eq!(Result::Success, result);
        assert_eq!(b"\x1B[?1;1$y", &buf[..written]);
    }

    #[test]
    fn encode_dec_mode_reset() {
        let mut buf = [0u8; modes::Report::MAX_SIZE];
        let mut written: usize = 0;
        let result = unsafe { report_encode(tag(1, false), 2, buf.as_mut_ptr(), buf.len(), &mut written) };
        assert_eq!(Result::Success, result);
        assert_eq!(b"\x1B[?1;2$y", &buf[..written]);
    }

    #[test]
    fn encode_ansi_mode() {
        let mut buf = [0u8; modes::Report::MAX_SIZE];
        let mut written: usize = 0;
        let result = unsafe { report_encode(tag(4, true), 1, buf.as_mut_ptr(), buf.len(), &mut written) };
        assert_eq!(Result::Success, result);
        assert_eq!(b"\x1B[4;1$y", &buf[..written]);
    }

    #[test]
    fn encode_not_recognized() {
        let mut buf = [0u8; modes::Report::MAX_SIZE];
        let mut written: usize = 0;
        let result = unsafe { report_encode(tag(9999, false), 0, buf.as_mut_ptr(), buf.len(), &mut written) };
        assert_eq!(Result::Success, result);
        assert_eq!(b"\x1B[?9999;0$y", &buf[..written]);
    }

    #[test]
    fn encode_with_insufficient_buffer() {
        let mut buf = [0u8; 1];
        let mut written: usize = 0;
        let result = unsafe { report_encode(tag(1, false), 1, buf.as_mut_ptr(), buf.len(), &mut written) };
        assert_eq!(Result::OutOfSpace, result);
        assert!(written > 1);
    }

    #[test]
    fn encode_with_invalid_state() {
        let mut buf = [0u8; modes::Report::MAX_SIZE];
        let mut written: usize = 0;
        let result = unsafe { report_encode(tag(1, false), 99, buf.as_mut_ptr(), buf.len(), &mut written) };
        assert_eq!(Result::InvalidValue, result);
    }

    #[test]
    fn encode_with_null_buffer() {
        let mut written: usize = 0;
        let result = unsafe { report_encode(tag(1, false), 1, std::ptr::null_mut(), 0, &mut written) };
        assert_eq!(Result::OutOfSpace, result);
        assert!(written > 0);
    }
}
