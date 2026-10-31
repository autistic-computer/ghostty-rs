//! Port of `src/config/ClipboardCodepointMap.zig`.
//!
//! STATUS: ported
//!
//! ClipboardCodepointMap is a map of codepoints to replacement values
//! for clipboard operations. When copying text to clipboard, matching
//! codepoints will be replaced with their mapped values.

/// To ease our usage later, we map it directly to formatter entries.
pub use crate::terminal::formatter::CodepointMap as Entry;

/// (Zig: `Entry.Replacement`.)
pub use crate::terminal::formatter::Replacement;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClipboardCodepointMap {
    /// The list of entries. We use a multiarraylist for cache-friendly lookups.
    ///
    /// Note: we do a linear search because we expect to always have very
    /// few entries, so the overhead of a binary search is not worth it.
    pub list: Vec<Entry>,
}

impl ClipboardCodepointMap {
    pub fn deinit(&mut self) {
        self.list = Vec::new();
    }

    /// Deep copy of the struct. The given allocator is expected to
    /// be an arena allocator of some sort since the struct itself
    /// doesn't support fine-grained deallocation of fields.
    pub fn clone_map(&self) -> ClipboardCodepointMap {
        // Strings are owned, so a derived clone is a deep copy.
        self.clone()
    }

    /// Add an entry to the map.
    ///
    /// For conflicting codepoints, entries added later take priority over
    /// entries added earlier.
    pub fn add(&mut self, entry: Entry) {
        debug_assert!(entry.range[0] <= entry.range[1]);
        self.list.push(entry);
    }
}
