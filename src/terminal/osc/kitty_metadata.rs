//! Port of `src/terminal/osc/kitty_metadata.zig`.
//!
//! STATUS: ported
//!
//! Helpers for parsing metadata shared by Kitty OSC protocols.
//!
//! Kitty OSC 99 and OSC 5522 encode metadata as colon-separated `key=value`
//! fields. The iterator in this module lazily searches that metadata for one
//! key, preserving the order of repeated values without allocating.
//!
//! Parsing is intentionally tolerant. Whitespace around keys and values is
//! trimmed, while malformed fields, non-matching keys, and invalid values are
//! skipped. Returned values are slices of the original metadata and remain
//! valid only as long as that input remains valid.

use crate::terminal::color::trim;

/// Zig `std.ascii.whitespace`.
pub(crate) const ASCII_WHITESPACE: &[u8] = b" \t\n\r\x0b\x0c";

/// An iterator over values whose key exactly matches `key`.
///
/// If `valid_value_characters` is non-null, every byte in a returned value must
/// appear in that character set. Passing null disables value validation.
///
/// In Zig, `key` and `valid_value_characters` are comptime parameters of the
/// type; here they are stored in the iterator.
#[derive(Debug, Clone)]
pub struct ValueIterator<'a> {
    key: &'static [u8],
    valid_value_characters: Option<&'static [u8]>,
    metadata: &'a [u8],
    pos: usize,
}

impl<'a> ValueIterator<'a> {
    /// Initialize an iterator borrowing `metadata`.
    pub fn init(
        key: &'static [u8],
        valid_value_characters: Option<&'static [u8]>,
        metadata: &'a [u8],
    ) -> Self {
        ValueIterator {
            key,
            valid_value_characters,
            metadata,
            pos: 0,
        }
    }

    /// Return the next valid matching value, or null when none remain.
    /// The returned slice borrows the metadata passed to `init`.
    pub fn next(&mut self) -> Option<&'a [u8]> {
        while self.pos < self.metadata.len() {
            let end = self.metadata[self.pos..]
                .iter()
                .position(|&c| c == b':')
                .map(|p| p + self.pos)
                .unwrap_or(self.metadata.len());
            let field = &self.metadata[self.pos..end];
            self.pos = if end < self.metadata.len() { end + 1 } else { end };

            let Some(equals) = field.iter().position(|&c| c == b'=') else {
                continue;
            };
            let field_key = trim(&field[..equals], ASCII_WHITESPACE);
            if field_key != self.key {
                continue;
            }

            let value = trim(&field[equals + 1..], ASCII_WHITESPACE);
            if let Some(valid) = self.valid_value_characters {
                if value.iter().any(|c| !valid.contains(c)) {
                    continue;
                }
            }

            return Some(value);
        }

        None
    }
}

impl<'a> Iterator for ValueIterator<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<&'a [u8]> {
        ValueIterator::next(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_iterator_skips_malformed_and_prefix_matching_keys() {
        let mut it = ValueIterator::init(b"id", None, b"id-extra=wrong:id: id = first :id=second");

        assert_eq!(b"first", it.next().unwrap());
        assert_eq!(b"second", it.next().unwrap());
        assert!(it.next().is_none());
    }

    #[test]
    fn value_iterator_skips_values_containing_disallowed_characters() {
        let mut it = ValueIterator::init(b"i", Some(b"abc"), b"i=a?:i=abc");

        assert_eq!(b"abc", it.next().unwrap());
        assert!(it.next().is_none());
    }
}
