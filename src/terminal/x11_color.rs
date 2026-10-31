//! Port of `src/terminal/x11_color.zig`.
//!
//! STATUS: ported
//!
//! The Zig version builds `entries` and `map` at comptime. Rust builds them
//! lazily on first use (`LazyLock`) from the same embedded `rgb.txt`, with
//! identical contents.

use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::sync::LazyLock;

use crate::terminal::color::Rgb;

/// A single X11 color entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    /// Color name. Null-terminated so it can be exposed through the C API
    /// without runtime allocation.
    pub name: &'static CStr,
    pub color: Rgb,
}

/// All X11 colors in rgb.txt file order.
///
/// This is kept separate from `map` because the C API needs stable file order
/// and null-terminated names. `ColorMap` keys do not provide that contract.
pub static ENTRIES: LazyLock<Vec<Entry>> = LazyLock::new(entries_array);

/// The map of all available X11 colors.
pub static MAP: LazyLock<ColorMap> = LazyLock::new(color_map);

/// Convenience accessor matching the Zig `entries` slice.
pub fn entries() -> &'static [Entry] {
    &ENTRIES
}

/// A static string map with ASCII case-insensitive key comparison
/// (Zig: `std.StaticStringMapWithEql(RGB, eqlAsciiIgnoreCase)`).
#[derive(Debug)]
pub struct ColorMap {
    /// Keys are stored ASCII-lowercased.
    map: HashMap<Box<[u8]>, Rgb>,
    /// The longest key, so lookups of longer strings fail without
    /// allocating.
    max_len: usize,
}

impl ColorMap {
    /// Look up a color by name, ignoring ASCII case.
    pub fn get(&self, key: &[u8]) -> Option<Rgb> {
        if key.len() > self.max_len {
            return None;
        }
        let mut buf = [0u8; 64];
        let lower = &mut buf[..key.len()];
        for (dst, src) in lower.iter_mut().zip(key) {
            *dst = src.to_ascii_lowercase();
        }
        self.map.get(&*lower).copied()
    }
}

fn entries_array() -> Vec<Entry> {
    let len = DATA.bytes().filter(|&b| b == b'\n').count();
    let mut result = Vec::with_capacity(len);
    // Parse the line. This is not very robust parsing, because we expect
    // a very exact format for rgb.txt. However, if our data is bad, one of
    // our unit tests will catch it.
    for raw_line in DATA.split('\n') {
        // Trim \r so this works with both LF and CRLF line endings,
        // since git may convert rgb.txt to CRLF on Windows checkouts.
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.is_empty() {
            continue;
        }
        let r: u8 = line[0..3].trim_matches(' ').parse().unwrap();
        let g: u8 = line[4..7].trim_matches(' ').parse().unwrap();
        let b: u8 = line[8..11].trim_matches(' ').parse().unwrap();
        let name = line[12..].trim_matches(|c| c == ' ' || c == '\t');
        let name: &'static CStr = Box::leak(CString::new(name).unwrap().into_boxed_c_str());
        result.push(Entry {
            name,
            color: Rgb { r, g, b },
        });
    }
    debug_assert_eq!(result.len(), len);
    result
}

fn color_map() -> ColorMap {
    let mut map = HashMap::with_capacity(ENTRIES.len());
    let mut max_len = 0;
    for entry in ENTRIES.iter() {
        let key: Box<[u8]> = entry.name.to_bytes().to_ascii_lowercase().into_boxed_slice();
        max_len = max_len.max(key.len());
        map.entry(key).or_insert(entry.color);
    }
    assert!(max_len <= 64);
    ColorMap { map, max_len }
}

/// This is the rgb.txt file from the X11 project. This was last sourced
/// from this location: https://gitlab.freedesktop.org/xorg/app/rgb
/// This data is licensed under the MIT/X11 license while this file is
/// licensed under the same license as Ghostty.
const DATA: &str = include_str!("res/rgb.txt");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_map() {
        assert_eq!(None, MAP.get(b"nosuchcolor"));
        assert_eq!(Rgb { r: 255, g: 255, b: 255 }, MAP.get(b"white").unwrap());
        assert_eq!(Some(Rgb { r: 0, g: 250, b: 154 }), MAP.get(b"medium spring green"));
        assert_eq!(Some(Rgb { r: 34, g: 139, b: 34 }), MAP.get(b"ForestGreen"));
        assert_eq!(Some(Rgb { r: 34, g: 139, b: 34 }), MAP.get(b"FoReStGReen"));
        assert_eq!(Some(Rgb { r: 0, g: 0, b: 0 }), MAP.get(b"black"));
        assert_eq!(Some(Rgb { r: 255, g: 0, b: 0 }), MAP.get(b"red"));
        assert_eq!(Some(Rgb { r: 0, g: 255, b: 0 }), MAP.get(b"green"));
        assert_eq!(Some(Rgb { r: 0, g: 0, b: 255 }), MAP.get(b"blue"));
        assert_eq!(Some(Rgb { r: 255, g: 255, b: 255 }), MAP.get(b"white"));
        assert_eq!(Some(Rgb { r: 124, g: 252, b: 0 }), MAP.get(b"lawngreen"));
        assert_eq!(Some(Rgb { r: 0, g: 250, b: 154 }), MAP.get(b"mediumspringgreen"));
        assert_eq!(Some(Rgb { r: 34, g: 139, b: 34 }), MAP.get(b"forestgreen"));
    }

    #[test]
    fn test_entries() {
        let entries = entries();
        assert!(entries.len() > 700);
        for entry in entries {
            assert_eq!(entry.color, MAP.get(entry.name.to_bytes()).unwrap());
            // Null-terminated.
            assert_eq!(0u8, entry.name.to_bytes_with_nul()[entry.name.to_bytes().len()]);
        }
        assert_eq!(b"snow", entries[0].name.to_bytes());
    }
}
