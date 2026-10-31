//! Port of `src/terminal/UTF8Decoder.zig`.
//!
//! STATUS: ported
//!
//! DFA-based non-allocating error-replacing UTF-8 decoder.
//!
//! This implementation is based largely on the excellent work of
//! Bjoern Hoehrmann, with slight modifications to support error-
//! replacement.
//!
//! For details on Bjoern's DFA-based UTF-8 decoder, see
//! http://bjoern.hoehrmann.de/utf-8/decoder/dfa (MIT licensed)

#[rustfmt::skip]
static CHAR_CLASSES: [u8; 256] = [
   0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,  0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
   0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,  0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
   0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,  0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
   0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,  0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
   1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,  9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,
   7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,  7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,
   8,8,2,2,2,2,2,2,2,2,2,2,2,2,2,2,  2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,
  10,3,3,3,3,3,3,3,3,3,3,3,3,4,3,3, 11,6,6,6,5,8,8,8,8,8,8,8,8,8,8,8,
];

#[rustfmt::skip]
static TRANSITIONS: [u8; 108] = [
   0,12,24,36,60,96,84,12,12,12,48,72, 12,12,12,12,12,12,12,12,12,12,12,12,
  12, 0,12,12,12,12,12, 0,12, 0,12,12, 12,24,12,12,12,12,12,24,12,24,12,12,
  12,12,12,12,12,12,12,24,12,12,12,12, 12,24,12,12,12,12,12,12,12,24,12,12,
  12,12,12,12,12,12,12,36,12,36,12,12, 12,36,12,12,12,12,12,36,12,36,12,12,
  12,36,12,12,12,12,12,12,12,12,12,12,
];

// DFA states
const ACCEPT_STATE: u8 = 0;
const REJECT_STATE: u8 = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Utf8Decoder {
    /// This is where we accumulate our current codepoint (a u21 in Zig).
    pub accumulator: u32,
    /// The internal state of the DFA.
    pub state: u8,
}

impl Utf8Decoder {
    /// Takes the next byte in the utf-8 sequence and emits a tuple of
    /// - The codepoint that was generated, if there is one.
    /// - A boolean that indicates whether the provided byte was consumed.
    ///
    /// The only case where the byte is not consumed is if an ill-formed
    /// sequence is reached, in which case a replacement character will be
    /// emitted and the byte will not be consumed.
    ///
    /// If the byte is not consumed, the caller is responsible for calling
    /// again with the same byte before continuing.
    #[inline(always)]
    pub fn next(&mut self, byte: u8) -> (Option<u32>, bool) {
        let char_class = CHAR_CLASSES[byte as usize];

        let initial_state = self.state;

        if self.state != ACCEPT_STATE {
            // u21 arithmetic: the shift drops bits beyond 21.
            self.accumulator = (self.accumulator << 6) & 0x1F_FFFF;
            self.accumulator |= (byte & 0x3F) as u32;
        } else {
            self.accumulator = (0xFFu32 >> char_class) & (byte as u32);
        }

        self.state = TRANSITIONS[(self.state + char_class) as usize];

        if self.state == ACCEPT_STATE {
            let cp = self.accumulator;
            self.accumulator = 0;

            // Emit the fully decoded codepoint.
            (Some(cp), true)
        } else if self.state == REJECT_STATE {
            self.accumulator = 0;
            self.state = ACCEPT_STATE;
            // Emit a replacement character. If we rejected the first byte
            // in a sequence, then it was consumed, otherwise it was not.
            (Some(0xFFFD), initial_state == ACCEPT_STATE)
        } else {
            // Emit nothing, we're in the middle of a sequence.
            (None, true)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii() {
        let mut d = Utf8Decoder::default();
        let mut out = [0u8; 13];
        for (i, &byte) in b"Hello, World!".iter().enumerate() {
            let res = d.next(byte);
            assert!(res.1);
            if let Some(codepoint) = res.0 {
                out[i] = codepoint as u8;
            }
        }

        assert_eq!(&out, b"Hello, World!");
    }

    #[test]
    fn well_formed_utf_8() {
        let mut d = Utf8Decoder::default();
        let mut out = [0u32; 4];
        let mut i = 0;
        // 4 bytes, 3 bytes, 2 bytes, 1 byte
        for &byte in "😄✤ÁA".as_bytes() {
            let mut consumed = false;
            while !consumed {
                let res = d.next(byte);
                consumed = res.1;
                // There are no errors in this sequence, so
                // every byte should be consumed first try.
                assert!(consumed);
                if let Some(codepoint) = res.0 {
                    out[i] = codepoint;
                    i += 1;
                }
            }
        }

        assert_eq!(out, [0x1F604, 0x2724, 0xC1, 0x41]);
    }

    #[test]
    fn partially_invalid_utf_8() {
        let mut d = Utf8Decoder::default();
        let mut out = [0u32; 5];
        let mut i = 0;
        // Illegally terminated sequence, valid sequence, illegal surrogate pair.
        for &byte in b"\xF0\x9F\xF0\x9F\x98\x84\xED\xA0\x80" {
            let mut consumed = false;
            while !consumed {
                let res = d.next(byte);
                consumed = res.1;
                if let Some(codepoint) = res.0 {
                    out[i] = codepoint;
                    i += 1;
                }
            }
        }

        assert_eq!(out, [0xFFFD, 0x1F604, 0xFFFD, 0xFFFD, 0xFFFD]);
    }
}
