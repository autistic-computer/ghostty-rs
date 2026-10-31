//! Port of `src/terminal/c/focus.zig`.
//!
//! STATUS: ported
//!
//! Notes on the port: `terminal.focus.Event` is a `lib.Enum` (`c_int` in C
//! ABI builds); the Rust core enum is `u8`-backed, so [`Event`] is its C
//! mirror and the extern function takes the raw `c_int` (an invalid value
//! returns `invalid_value`).

use std::ffi::c_int;

use super::result::Result;
use super::types::encode_to_buffer;
use crate::terminal::focus as terminal_focus;

crate::c_api_enum! {
    /// C: GhosttyFocusEvent
    pub enum Event {
        Gained = 0,
        Lost = 1,
    }
}

impl Event {
    pub fn to_zig(self) -> terminal_focus::Event {
        match self {
            Event::Gained => terminal_focus::Event::Gained,
            Event::Lost => terminal_focus::Event::Lost,
        }
    }
}

/// C: ghostty_focus_encode
///
/// # Safety
/// A non-null `out_` must be valid for `out_len` writable bytes.
#[export_name = "ghostty_focus_encode"]
pub unsafe extern "C" fn encode(event: c_int, out_: *mut u8, out_len: usize, out_written: &mut usize) -> Result {
    let Some(event) = Event::from_int(event) else {
        return Result::InvalidValue;
    };
    let event = event.to_zig();
    encode_to_buffer(out_, out_len, out_written, |w| terminal_focus::encode(w, event))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_focus_gained() {
        let mut buf = [0u8; terminal_focus::MAX_ENCODE_SIZE];
        let mut written: usize = 0;
        let result = unsafe { encode(Event::Gained as c_int, buf.as_mut_ptr(), buf.len(), &mut written) };
        assert_eq!(Result::Success, result);
        assert_eq!(b"\x1B[I", &buf[..written]);
    }

    #[test]
    fn encode_focus_lost() {
        let mut buf = [0u8; terminal_focus::MAX_ENCODE_SIZE];
        let mut written: usize = 0;
        let result = unsafe { encode(Event::Lost as c_int, buf.as_mut_ptr(), buf.len(), &mut written) };
        assert_eq!(Result::Success, result);
        assert_eq!(b"\x1B[O", &buf[..written]);
    }

    #[test]
    fn encode_with_insufficient_buffer() {
        let mut buf = [0u8; 1];
        let mut written: usize = 0;
        let result = unsafe { encode(Event::Gained as c_int, buf.as_mut_ptr(), buf.len(), &mut written) };
        assert_eq!(Result::OutOfSpace, result);
        assert_eq!(terminal_focus::MAX_ENCODE_SIZE, written);
    }

    #[test]
    fn encode_with_null_buffer() {
        let mut written: usize = 0;
        let result = unsafe { encode(Event::Gained as c_int, std::ptr::null_mut(), 0, &mut written) };
        assert_eq!(Result::OutOfSpace, result);
        assert_eq!(terminal_focus::MAX_ENCODE_SIZE, written);
    }
}
