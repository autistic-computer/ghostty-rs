//! Port of `src/terminal/focus.zig`.
//!
//! STATUS: ported

use std::io::Write;

/// Maximum number of bytes that `encode` will write. Any users of this
/// should be resilient to this changing, so this is always a specific
/// value (e.g. we don't add unnecessary padding).
pub const MAX_ENCODE_SIZE: usize = 3;

/// A focus event that can be reported to the application running in the
/// terminal when focus reporting mode (mode 1004) is enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Event {
    Gained = 0,
    Lost = 1,
}

/// Encode a focus in/out report (CSI I / CSI O).
pub fn encode<W: Write + ?Sized>(writer: &mut W, event: Event) -> std::io::Result<()> {
    writer.write_all(match event {
        Event::Gained => b"\x1B[I",
        Event::Lost => b"\x1B[O",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_focus_gained() {
        let mut buf = [0u8; MAX_ENCODE_SIZE];
        let mut writer: &mut [u8] = &mut buf;
        encode(&mut writer, Event::Gained).unwrap();
        let n = MAX_ENCODE_SIZE - writer.len();
        assert_eq!(b"\x1B[I", &buf[..n]);
    }

    #[test]
    fn encode_focus_lost() {
        let mut buf = [0u8; MAX_ENCODE_SIZE];
        let mut writer: &mut [u8] = &mut buf;
        encode(&mut writer, Event::Lost).unwrap();
        let n = MAX_ENCODE_SIZE - writer.len();
        assert_eq!(b"\x1B[O", &buf[..n]);
    }
}
