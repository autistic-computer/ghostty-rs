//! Port of `src/font/CodepointMap.zig`.
//!
//! STATUS: ported
//!
//! CodepointMap is a map of codepoints to a discovery descriptor of a font
//! to use for that codepoint. If the descriptor doesn't return any matching
//! font, the codepoint is rendered using the default font.

use super::discovery;
use crate::terminal::hash_map::Wyhash;

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// Unicode codepoint range. Asserts range[0] <= range[1].
    pub range: [u32; 2],

    /// The discovery descriptor of the font to use for this range.
    pub descriptor: discovery::Descriptor,
}

/// The list of entries. We use a multiarraylist because Descriptors are
/// quite large and we will very rarely match, so we'd rather pack our
/// ranges together to make everything more cache friendly for lookups.
///
/// Note: we just do a linear search because we expect to always have very
/// few entries, so the overhead of a binary search is not worth it. This is
/// possible to defeat with some pathological inputs, but there is no realistic
/// scenario where this will be a problem except people trying to fuck around.
///
/// The Zig `std.MultiArrayList(Entry)` is represented as two parallel
/// vectors (struct-of-arrays) with the same length.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CodepointMap {
    ranges: Vec<[u32; 2]>,
    descriptors: Vec<discovery::Descriptor>,
}

impl CodepointMap {
    pub fn new() -> CodepointMap {
        CodepointMap::default()
    }

    /// Number of entries in the map.
    pub fn len(&self) -> usize {
        self.ranges.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// Deep copy of the struct.
    pub fn clone_map(&self) -> CodepointMap {
        self.clone()
    }

    /// Add an entry to the map.
    ///
    /// For conflicting codepoints, entries added later take priority over
    /// entries added earlier.
    pub fn add(&mut self, entry: Entry) {
        debug_assert!(entry.range[0] <= entry.range[1]);
        self.ranges.push(entry.range);
        self.descriptors.push(entry.descriptor);
    }

    /// The codepoint ranges, in insertion order (Zig
    /// `list.items(.range)`).
    pub fn ranges(&self) -> &[[u32; 2]] {
        &self.ranges
    }

    /// The descriptors, in insertion order (Zig
    /// `list.items(.descriptor)`).
    pub fn descriptors(&self) -> &[discovery::Descriptor] {
        &self.descriptors
    }

    /// The entry at index `i` (Zig `list.get(i)`).
    pub fn entry(&self, i: usize) -> Entry {
        Entry {
            range: self.ranges[i],
            descriptor: self.descriptors[i].clone(),
        }
    }

    /// Get a descriptor for a codepoint.
    pub fn get(&self, cp: u32) -> Option<&discovery::Descriptor> {
        let items = &self.ranges;
        for forward_i in 0..items.len() {
            let i = items.len() - forward_i - 1;
            let range = items[i];
            if range[0] <= cp && cp <= range[1] {
                return Some(&self.descriptors[i]);
            }
        }

        None
    }

    /// Hash with the given hasher.
    pub fn hash<H: std::hash::Hasher>(&self, hasher: &mut H) {
        hasher.write(&(self.ranges.len() as u64).to_le_bytes());
        for (range, descriptor) in self.ranges.iter().zip(self.descriptors.iter()) {
            // autoHash of a [2]u21: each element as its u21 (padded) bytes.
            hasher.write(&range[0].to_le_bytes());
            hasher.write(&range[1].to_le_bytes());
            descriptor.hash(hasher);
        }
    }

    /// Returns a hash code that can be used to uniquely identify this
    /// action.
    pub fn hashcode(&self) -> u64 {
        let mut hasher = Wyhash::init(0);
        self.hash(&mut hasher);
        hasher.final_()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desc(family: &str) -> discovery::Descriptor {
        discovery::Descriptor {
            family: Some(family.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn codepointmap() {
        let mut m = CodepointMap::new();

        // Exact range
        assert!(m.get(1).is_none());
        m.add(Entry {
            range: [1, 1],
            descriptor: desc("A"),
        });
        {
            let d = m.get(1).unwrap();
            assert_eq!("A", d.family.as_deref().unwrap());
        }

        // Later entry takes priority
        m.add(Entry {
            range: [1, 2],
            descriptor: desc("B"),
        });
        {
            let d = m.get(1).unwrap();
            assert_eq!("B", d.family.as_deref().unwrap());
        }

        // Non-matching
        assert!(m.get(0).is_none());
        assert!(m.get(3).is_none());

        m.add(Entry {
            range: [3, 4],
            descriptor: desc("C"),
        });
        m.add(Entry {
            range: [5, 6],
            descriptor: desc("D"),
        });
        {
            let d = m.get(3).unwrap();
            assert_eq!("C", d.family.as_deref().unwrap());
        }
        {
            let d = m.get(1).unwrap();
            assert_eq!("B", d.family.as_deref().unwrap());
        }

        // Clone and hash are consistent.
        let c = m.clone_map();
        assert_eq!(m.hashcode(), c.hashcode());
        assert_eq!(c.get(5).unwrap().family.as_deref(), Some("D"));
    }
}
