//! Port of `src/font/shaper/feature.zig`.
//!
//! STATUS: ported

use std::fmt;
use std::io::{Cursor, Read};

/// Represents an OpenType font feature setting, which consists of a tag and
/// a numeric parameter >= 0. Most features are boolean, so only parameters
/// of 0 and 1 make sense for them, but some (e.g. 'cv01'..'cv99') can take
/// parameters to choose between multiple variants of a given character or
/// characters.
///
/// Ref:
/// - https://learn.microsoft.com/en-us/typography/opentype/spec/chapter2#features-and-lookups
/// - https://harfbuzz.github.io/shaping-opentype-features.html
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Feature {
    pub tag: [u8; 4],
    pub value: u32,
}

/// Error returned by [`Feature::to_string_buf`] when the buffer is too small.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoSpaceLeft;

impl fmt::Display for NoSpaceLeft {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NoSpaceLeft")
    }
}

impl std::error::Error for NoSpaceLeft {}

/// Parser states for [`Feature::from_reader`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Initial state.
    Start,
    /// Parsing the tag.
    Tag,
    /// In the space between the tag and the value.
    Space,
    /// Parsing an integer parameter directly in to `value`.
    Int,
    /// Parsing a boolean keyword parameter ("on"/"off").
    Bool,
    /// After the value has been parsed.
    Done,
    /// Fast-forwarding from an error to the next boundary.
    Err,
}

/// Read a single byte; end of stream (or any read error) is reported as
/// `None`.
fn read_byte<R: Read>(reader: &mut R) -> Option<u8> {
    let mut b = [0u8; 1];
    match reader.read(&mut b) {
        Ok(1) => Some(b[0]),
        _ => None,
    }
}

impl Feature {
    pub fn from_string(str: &[u8]) -> Option<Feature> {
        let mut reader = Cursor::new(str);
        Feature::from_reader(&mut reader)
    }

    /// Parse a single font feature setting from a reader, with a version
    /// of the syntax of HarfBuzz's font feature strings. Stops at end of stream
    /// or when a ',' is encountered.
    ///
    /// This parsing aims to be as error-tolerant as possible while avoiding any
    /// assumptions in ambiguous scenarios. When invalid syntax is encountered,
    /// the reader is advanced to the next boundary (end-of-stream or ',') so
    /// that further features may be read.
    ///
    /// Ref: https://harfbuzz.github.io/harfbuzz-hb-common.html#hb-feature-from-string
    pub fn from_reader<R: Read>(reader: &mut R) -> Option<Feature> {
        let mut tag_buf = [0u8; 4];
        let mut tag_len: usize = 0;
        let mut value: Option<u32> = None;

        // End of stream is treated as a ',' boundary in every state.
        let next = |reader: &mut R| read_byte(reader).unwrap_or(b',');

        let mut state = State::Start;
        'state: loop {
            match state {
                // If we're done then we skip whitespace until we see a ','.
                State::Done => loop {
                    match next(reader) {
                        b' ' | b'\t' => continue,
                        b',' => break 'state,
                        // If we see something other than whitespace or a ','
                        // then this is an error since the intent is unclear.
                        _ => {
                            state = State::Err;
                            continue 'state;
                        }
                    }
                },

                // If we're fast-forwarding from an error we just wanna
                // stop at the first boundary and ignore all other bytes.
                State::Err => {
                    while let Some(b) = read_byte(reader) {
                        if b == b',' {
                            break;
                        }
                    }
                    return None;
                }

                State::Start => loop {
                    match next(reader) {
                        // Ignore leading whitespace.
                        b' ' | b'\t' => continue,
                        // Empty feature string.
                        b',' => return None,
                        // '+' prefix to explicitly enable feature.
                        b'+' => {
                            value = Some(1);
                            state = State::Tag;
                            continue 'state;
                        }
                        // '-' prefix to explicitly disable feature.
                        b'-' => {
                            value = Some(0);
                            state = State::Tag;
                            continue 'state;
                        }
                        // Quote mark introducing a tag.
                        b'"' | b'\'' => {
                            state = State::Tag;
                            continue 'state;
                        }
                        // First letter of tag.
                        byte => {
                            tag_len = 1;
                            tag_buf[0] = byte;
                            state = State::Tag;
                            continue 'state;
                        }
                    }
                },

                State::Tag => loop {
                    match next(reader) {
                        // If the tag is interrupted by a comma it's invalid.
                        b',' => return None,
                        // Ignore quote marks. This does technically ignore cases like
                        // "'k'e'r'n' = 0", but it's unambiguous so if someone really
                        // wants to do that in their config then... sure why not.
                        b'"' | b'\'' => continue,
                        // In all other cases we add the byte to our tag.
                        byte => {
                            tag_len += 1;
                            tag_buf[tag_len - 1] = byte;
                            if tag_len == 4 {
                                state = State::Space;
                                continue 'state;
                            }
                        }
                    }
                },

                State::Space => loop {
                    match next(reader) {
                        b' ' | b'\t' => continue,
                        // Ignore quote marks since we might have a
                        // closing quote from the tag still ahead.
                        b'"' | b'\'' => continue,
                        // Allow an '=' (which we can safely ignore)
                        // only if we don't already have a value due
                        // to a '+' or '-' prefix.
                        b'=' => {
                            if value.is_some() {
                                state = State::Err;
                                continue 'state;
                            }
                        }
                        b',' => {
                            // Specifying only a tag turns a feature on.
                            if value.is_none() {
                                value = Some(1);
                            }
                            break 'state;
                        }
                        byte @ b'0'..=b'9' => {
                            // If we already have value because of a
                            // '+' or '-' prefix then this is an error.
                            if value.is_some() {
                                state = State::Err;
                                continue 'state;
                            }
                            value = Some((byte - b'0') as u32);
                            state = State::Int;
                            continue 'state;
                        }
                        b'o' | b'O' => {
                            // If we already have value because of a
                            // '+' or '-' prefix then this is an error.
                            if value.is_some() {
                                state = State::Err;
                                continue 'state;
                            }
                            state = State::Bool;
                            continue 'state;
                        }
                        _ => {
                            state = State::Err;
                            continue 'state;
                        }
                    }
                },

                State::Int => loop {
                    match next(reader) {
                        b',' => break 'state,
                        byte @ b'0'..=b'9' => {
                            // If our value gets too big while
                            // parsing we consider it an error.
                            //
                            // Port note: the addition is checked too (Zig
                            // would trip a safety check on that overflow).
                            match value
                                .unwrap()
                                .checked_mul(10)
                                .and_then(|v| v.checked_add((byte - b'0') as u32))
                            {
                                Some(v) => value = Some(v),
                                None => {
                                    state = State::Err;
                                    continue 'state;
                                }
                            }
                        }
                        _ => {
                            state = State::Err;
                            continue 'state;
                        }
                    }
                },

                State::Bool => loop {
                    match next(reader) {
                        b',' => return None,
                        b'n' | b'N' => {
                            // "ofn"
                            if value.is_some() {
                                debug_assert!(value == Some(0));
                                state = State::Err;
                                continue 'state;
                            }
                            value = Some(1);
                            state = State::Done;
                            continue 'state;
                        }
                        b'f' | b'F' => {
                            // To make sure we consume two 'f's.
                            if value.is_none() {
                                value = Some(0);
                            } else {
                                debug_assert!(value == Some(0));
                                state = State::Done;
                                continue 'state;
                            }
                        }
                        _ => {
                            state = State::Err;
                            continue 'state;
                        }
                    }
                },
            }
        }

        debug_assert!(value.is_some());
        debug_assert!(tag_len == 4);

        Some(Feature {
            tag: tag_buf,
            value: value.unwrap(),
        })
    }

    /// Serialize this feature to the provided buffer.
    /// The string that this produces should be valid to parse.
    ///
    /// (Zig `toString`; renamed so it does not shadow `ToString::to_string`,
    /// which is also available through the `Display` impl.)
    pub fn to_string_buf(&self, buf: &mut [u8]) -> Result<(), NoSpaceLeft> {
        use std::io::Write;
        let mut fbs = Cursor::new(buf);
        write!(fbs, "{}", self).map_err(|_| NoSpaceLeft)
    }
}

/// Formatter for logging
impl fmt::Display for Feature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let tag = String::from_utf8_lossy(&self.tag);
        if self.value <= 1 {
            // Format boolean options as "+tag" for on and "-tag" for off.
            write!(f, "{}{}", b"-+"[self.value as usize] as char, tag)
        } else {
            // Format non-boolean tags as "tag=value".
            write!(f, "{}={}", tag, self.value)
        }
    }
}

/// A list of font feature settings (see `Feature` for more documentation).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FeatureList {
    pub features: Vec<Feature>,
}

impl FeatureList {
    /// Parse a comma separated list of features.
    /// See `Feature::from_reader` for more docs.
    pub fn from_string(str: &[u8]) -> FeatureList {
        let mut this = FeatureList::default();
        this.append_from_string(str);
        this
    }

    /// Append features to this list from a string with a comma separated list.
    /// See `Feature::from_reader` for more docs.
    pub fn append_from_string(&mut self, str: &[u8]) {
        let mut reader = Cursor::new(str);
        while (reader.position() as usize) < str.len() {
            let i = reader.position() as usize;
            if let Some(feature) = Feature::from_reader(&mut reader) {
                self.features.push(feature);
            } else {
                log::warn!(
                    target: "font_shaper",
                    "failed to parse font feature setting: \"{}\"",
                    String::from_utf8_lossy(&str[i..reader.position() as usize]),
                );
            }
        }
    }
}

/// Formatter for logging
///
/// Port note: the Zig `FeatureList.format` body is not valid (it refers
/// to `self.value`/`self.tag`, which only exist on `Feature`) and is never
/// instantiated; this port keeps its evident intent: the features
/// separated by ", ".
impl fmt::Display for FeatureList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, feature) in self.features.iter().enumerate() {
            write!(f, "{}", feature)?;
            if i != self.features.len() - 1 {
                f.write_str(", ")?;
            }
        }
        Ok(())
    }
}

/// These features are hardcoded to always be on by default. Users
/// can turn them off by setting the features to "-liga" for example.
pub const DEFAULT_FEATURES: [Feature; 1] = [Feature {
    tag: *b"liga",
    value: 1,
}];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_from_string() {
        // This is not *complete* coverage of every possible
        // combination of syntax, but it covers quite a few.

        // Boolean settings (on)
        let kern_on = Some(Feature { tag: *b"kern", value: 1 });
        assert_eq!(kern_on, Feature::from_string(b"kern"));
        assert_eq!(kern_on, Feature::from_string(b"kern, "));
        assert_eq!(kern_on, Feature::from_string(b"kern on"));
        assert_eq!(kern_on, Feature::from_string(b"kern on, "));
        assert_eq!(kern_on, Feature::from_string(b"+kern"));
        assert_eq!(kern_on, Feature::from_string(b"+kern, "));
        assert_eq!(kern_on, Feature::from_string(b"\"kern\" = 1"));
        assert_eq!(kern_on, Feature::from_string(b"\"kern\" = 1, "));

        // Boolean settings (off)
        let kern_off = Some(Feature { tag: *b"kern", value: 0 });
        assert_eq!(kern_off, Feature::from_string(b"kern off"));
        assert_eq!(kern_off, Feature::from_string(b"kern off, "));
        assert_eq!(kern_off, Feature::from_string(b"-'kern'"));
        assert_eq!(kern_off, Feature::from_string(b"-'kern', "));
        assert_eq!(kern_off, Feature::from_string(b"\"kern\" = 0"));
        assert_eq!(kern_off, Feature::from_string(b"\"kern\" = 0, "));

        // Non-boolean settings
        let aalt_2 = Some(Feature { tag: *b"aalt", value: 2 });
        assert_eq!(aalt_2, Feature::from_string(b"aalt=2"));
        assert_eq!(aalt_2, Feature::from_string(b"aalt=2, "));
        assert_eq!(aalt_2, Feature::from_string(b"'aalt' 2"));
        assert_eq!(aalt_2, Feature::from_string(b"'aalt' 2, "));

        // Various ambiguous/error cases which should be null
        assert_eq!(None, Feature::from_string(b"aalt=2x")); // bad number
        assert_eq!(None, Feature::from_string(b"toolong")); // tag too long
        assert_eq!(None, Feature::from_string(b"sht")); // tag too short
        assert_eq!(None, Feature::from_string(b"-kern 1")); // redundant/conflicting
        assert_eq!(None, Feature::from_string(b"-kern on")); // redundant/conflicting
        assert_eq!(None, Feature::from_string(b"aalt=o,")); // bad keyword
        assert_eq!(None, Feature::from_string(b"aalt=ofn,")); // bad keyword
    }

    #[test]
    fn feature_list_from_string() {
        let str = concat!(
            "  kern, kern on , +kern, \"kern\"  = 1,", // Boolean settings (on)
            "kern    off, -'kern' , \"kern\"=0,",      // Boolean settings (off)
            "aalt=2,  'aalt'\t2,",                     // Non-boolean settings
            "aalt=2x, toolong, sht, -kern 1, -kern on, aalt=o, aalt=ofn,", // Invalid cases
            "last", // To ensure final element is included correctly.
        );
        let feats = FeatureList::from_string(str.as_bytes());

        let expected = [
            Feature { tag: *b"kern", value: 1 },
            Feature { tag: *b"kern", value: 1 },
            Feature { tag: *b"kern", value: 1 },
            Feature { tag: *b"kern", value: 1 },
            Feature { tag: *b"kern", value: 0 },
            Feature { tag: *b"kern", value: 0 },
            Feature { tag: *b"kern", value: 0 },
            Feature { tag: *b"aalt", value: 2 },
            Feature { tag: *b"aalt", value: 2 },
            Feature { tag: *b"last", value: 1 },
        ];

        assert_eq!(&expected[..], &feats.features[..]);
    }
}
