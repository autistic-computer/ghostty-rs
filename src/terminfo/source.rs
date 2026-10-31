//! Port of `src/terminfo/Source.zig`.
//!
//! STATUS: ported
//!
//! Terminfo source format. This can be used to encode terminfo files.
//! This cannot parse terminfo source files yet because it isn't something
//! I need to do but this can be added later.
//!
//! Background: https://invisible-island.net/ncurses/man/terminfo.5.html

use std::collections::HashMap;
use std::ffi::CString;
use std::io;

/// Terminfo source.
#[derive(Debug, Clone, Copy)]
pub struct Source {
    /// The set of names for the terminal. These match the TERM environment variable
    /// and are used to look up this terminal. Historically, the final name in the
    /// list was the most common name for the terminal and contains spaces and
    /// other characters. See terminfo(5) for details.
    pub names: &'static [&'static str],

    /// The set of capabilities in this terminfo file.
    pub capabilities: &'static [Capability],
}

/// A capability in a terminfo file. This also includes any "use" capabilities
/// since they behave just like other capabilities as documented in terminfo(5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capability {
    /// The name of capability. This is the "Cap-name" value in terminfo(5).
    pub name: &'static str,
    pub value: Value,
}

/// The value of a [`Capability`] (`Capability.Value` in Zig).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value {
    /// Canceled value, i.e. suffixed with @
    Canceled,

    /// Boolean values are always true if they exist so there is no value.
    Boolean,

    /// Numeric values are always "unsigned decimal integers". The size
    /// of the integer is unspecified in terminfo(5). I chose 32-bits
    /// because it is a common integer size but this may be wrong.
    Numeric(u32),

    String(&'static str),
}

/// The XTGETTCAP response map returned by [`Source::xtgettcap_map`].
///
/// Keys are the upper-case hex-encoded capability names (as sent by the
/// client in the XTGETTCAP request). Values are the FULL response including
/// the escape sequences. The responses are null-terminated (`CString`) so that
/// they can be handed directly to APIs that require a sentinel (e.g. the
/// libghostty-vt write_pty callback) without copying.
#[derive(Debug, Clone, Default)]
pub struct XtgettcapMap {
    map: HashMap<Vec<u8>, CString>,
}

impl XtgettcapMap {
    /// Look up the response for a hex-encoded capability name.
    pub fn get(&self, key: &[u8]) -> Option<&CString> {
        self.map.get(key)
    }

    /// Number of entries in the map.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// True if the map has no entries.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

impl Source {
    /// Encode as a terminfo source file. The encoding is always done in a
    /// human-readable format with whitespace. Fields are always written in the
    /// order of the slices on this struct; this will not do any reordering.
    pub fn encode<W: io::Write + ?Sized>(&self, writer: &mut W) -> io::Result<()> {
        // Encode the names in the order specified
        for (i, name) in self.names.iter().enumerate() {
            if i != 0 {
                writer.write_all(b"|")?;
            }
            writer.write_all(name.as_bytes())?;
        }
        writer.write_all(b",\n")?;

        // Encode each of the capabilities in the order specified
        for cap in self.capabilities {
            writer.write_all(b"\t")?;
            writer.write_all(cap.name.as_bytes())?;
            match cap.value {
                Value::Canceled => writer.write_all(b"@")?,
                Value::Boolean => {}
                Value::Numeric(v) => write!(writer, "#{v}")?,
                Value::String(v) => write!(writer, "={v}")?,
            }
            writer.write_all(b",\n")?;
        }

        Ok(())
    }

    /// Returns a map for all of the capabilities in this terminfo.
    /// The value is the value that should be sent as a response to XTGETTCAP.
    /// Important: the value is the FULL response included the escape sequences.
    ///
    /// The responses are null-terminated so that they can be handed directly
    /// to APIs that require a sentinel (e.g. the libghostty-vt write_pty
    /// callback) without copying.
    ///
    /// The Zig version builds a `StaticStringMap` at comptime; this builds the
    /// map at runtime (callers can cache it in a `static`). The comptime
    /// errors of the Zig version become panics.
    pub fn xtgettcap_map(&self) -> XtgettcapMap {
        // We have all of our capabilities plus To, TN, and RGB which aren't
        // in the capabilities list but are query-able.
        let len = self.capabilities.len() + 3;
        let mut kvs: Vec<(Vec<u8>, Vec<u8>)> = Vec::with_capacity(len);

        // We first build all of our entries with raw K=V pairs.
        kvs.push((b"TN".to_vec(), self.names[0].as_bytes().to_vec()));
        kvs.push((b"Co".to_vec(), b"256".to_vec()));
        kvs.push((b"RGB".to_vec(), b"8".to_vec()));
        for cap in self.capabilities {
            let value: Vec<u8> = match cap.value {
                Value::Canceled => panic!("canceled not handled yet"),
                Value::Boolean => Vec::new(),
                Value::String(v) => 'string: {
                    let v = v.as_bytes();
                    // If a string contains parameters, then we do not escape
                    // anything within the string. I BELIEVE the history here is
                    // xterm initially only supported specific capabilities and none
                    // had parameters so it returned the tcap encoded form. Later,
                    // Kitty added support for more capabilities some of which
                    // have parameters. But Kitty returned them in terminfo source
                    // format. So we need to handle both cases.
                    if v.contains(&b'%') {
                        break 'string v.to_vec();
                    }
                    // No-parameters. Encode and return.
                    // First replace \E with the escape char (0x1B)
                    let mut result = replace(v, b"\\E", b"\x1b");
                    // Replace '^' with the control char version of that char.
                    while let Some(idx) = result.iter().position(|&c| c == b'^') {
                        if idx > 0 {
                            panic!("handle control-char in middle of string");
                        }
                        let replacement = match result[idx + 1] {
                            b'?' => 0x7F, // DEL, special cased from ncurses
                            c => c - 64,
                        };
                        let needle = result[idx..idx + 2].to_vec();
                        result = replace(&result, &needle, &[replacement]);
                    }
                    result
                }
                Value::Numeric(v) => v.to_string().into_bytes(),
            };
            kvs.push((cap.name.as_bytes().to_vec(), value));
        }
        debug_assert_eq!(kvs.len(), len);

        // Now go through and convert them all to hex-encoded strings.
        let mut map = HashMap::with_capacity(len);
        for (key, value) in kvs {
            // The key is just the raw hex-encoded string
            let key = hexencode(&key);

            // The value is more complex.
            let mut out = Vec::with_capacity(5 + key.len() + 1 + value.len() * 2 + 2);
            out.extend_from_slice(b"\x1bP1+r");
            out.extend_from_slice(&key); // important: hex-encoded name
            if !value.is_empty() {
                out.push(b'=');
                out.extend_from_slice(&hexencode(&value));
            }
            out.extend_from_slice(b"\x1b\\");

            // The response never contains a NUL byte: everything that isn't
            // hex-encoded is a fixed escape sequence.
            let out = CString::new(out).expect("xtgettcap response has no NUL bytes");

            // Like StaticStringMap, the first entry for a key wins.
            map.entry(key).or_insert(out);
        }

        XtgettcapMap { map }
    }
}

/// Upper-case hex encoding (`std.fmt.bytesToHex(input, .upper)`).
fn hexencode(input: &[u8]) -> Vec<u8> {
    const CHARSET: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = Vec::with_capacity(input.len() * 2);
    for &b in input {
        out.push(CHARSET[(b >> 4) as usize]);
        out.push(CHARSET[(b & 0xF) as usize]);
    }
    out
}

/// `std.mem.replace`: replace all non-overlapping occurrences of needle.
/// (The Zig version is `comptimeReplace`, a comptime-only wrapper.)
fn replace(input: &[u8], needle: &[u8], replacement: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if input[i..].starts_with(needle) {
            out.extend_from_slice(replacement);
            i += needle.len();
        } else {
            out.push(input[i]);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xtgettcap_map() {
        let src = Source {
            names: &["ghostty", "xterm-ghostty", "Ghostty"],
            capabilities: &[
                Capability {
                    name: "am",
                    value: Value::Boolean,
                },
                Capability {
                    name: "colors",
                    value: Value::Numeric(256),
                },
                Capability {
                    name: "kx",
                    value: Value::String("^?"),
                },
                Capability {
                    name: "kbs",
                    value: Value::String("^H"),
                },
                Capability {
                    name: "kf1",
                    value: Value::String("\\EOP"),
                },
                Capability {
                    name: "Smulx",
                    value: Value::String("\\E[4:%p1%dm"),
                },
            ],
        };

        let map = src.xtgettcap_map();
        assert_eq!(
            b"\x1bP1+r616D\x1b\\",
            map.get(&hexencode(b"am")).unwrap().as_bytes()
        );
        assert_eq!(
            b"\x1bP1+r6B78=7F\x1b\\",
            map.get(&hexencode(b"kx")).unwrap().as_bytes()
        );
        assert_eq!(
            b"\x1bP1+r6B6273=08\x1b\\",
            map.get(&hexencode(b"kbs")).unwrap().as_bytes()
        );
        assert_eq!(
            b"\x1bP1+r6B6631=1B4F50\x1b\\",
            map.get(&hexencode(b"kf1")).unwrap().as_bytes()
        );
        assert_eq!(
            b"\x1bP1+r536D756C78=5C455B343A25703125646D\x1b\\",
            map.get(&hexencode(b"Smulx")).unwrap().as_bytes()
        );
    }

    #[test]
    fn encode() {
        let src = Source {
            names: &["ghostty", "xterm-ghostty", "Ghostty"],
            capabilities: &[
                Capability {
                    name: "am",
                    value: Value::Boolean,
                },
                Capability {
                    name: "ccc",
                    value: Value::Canceled,
                },
                Capability {
                    name: "colors",
                    value: Value::Numeric(256),
                },
                Capability {
                    name: "bel",
                    value: Value::String("^G"),
                },
            ],
        };

        // Encode
        let mut buf: Vec<u8> = Vec::new();
        src.encode(&mut buf).unwrap();

        let expected = concat!(
            "ghostty|xterm-ghostty|Ghostty,\n",
            "\tam,\n",
            "\tccc@,\n",
            "\tcolors#256,\n",
            "\tbel=^G,\n",
        );
        assert_eq!(expected.as_bytes(), &buf[..]);
    }
}
