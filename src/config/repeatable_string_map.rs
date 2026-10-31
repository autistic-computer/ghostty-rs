//! Port of `src/config/RepeatableStringMap.zig`.
//!
//! STATUS: ported
//!
//! RepeatableStringMap is a key/value that can be repeated to accumulate a
//! string map. This isn't called "StringMap" because I find that sometimes
//! leads to confusion that it _accepts_ a map such as JSON dict.

use crate::cli::args::{parse_cli_new_from_default, Error, ParseCli, StringZ};
use crate::config::formatter::{self, EntryFormatter, FormatEntry};

/// Zig `std.ArrayHashMapUnmanaged([:0]const u8, [:0]const u8, ...)`: an
/// insertion-ordered map. Stored as a vector of pairs since these maps are
/// small.
pub type Map = Vec<(StringZ, StringZ)>;

/// Zig `std.ascii.whitespace`.
const ASCII_WHITESPACE: &[char] = &[' ', '\t', '\n', '\r', '\x0b', '\x0c'];

#[derive(Debug, Clone, Default)]
pub struct RepeatableStringMap {
    pub map: Map,
}

impl RepeatableStringMap {
    /// The number of items in the map
    pub fn count(&self) -> usize {
        self.map.len()
    }

    /// Iterator over the entries in the map.
    pub fn iterator(&self) -> impl Iterator<Item = (&StringZ, &StringZ)> {
        self.map.iter().map(|(k, v)| (k, v))
    }

    /// Returns the value for `key`, if any.
    pub fn get(&self, key: &str) -> Option<&StringZ> {
        self.map
            .iter()
            .find(|(k, _)| k.as_str() == key)
            .map(|(_, v)| v)
    }

    /// Insert or replace (keeping the original position), like
    /// `ArrayHashMap.put`.
    fn put(&mut self, key: StringZ, value: StringZ) {
        match self.map.iter_mut().find(|(k, _)| *k == key) {
            Some(entry) => entry.1 = value,
            None => self.map.push((key, value)),
        }
    }

    /// Like `ArrayHashMap.orderedRemove`.
    fn ordered_remove(&mut self, key: &str) {
        if let Some(idx) = self.map.iter().position(|(k, _)| k.as_str() == key) {
            self.map.remove(idx);
        }
    }

    /// Compare if two of our value are requal. Required by Config.
    pub fn equal(&self, other: &RepeatableStringMap) -> bool {
        if self.map.len() != other.map.len() {
            return false;
        }
        for (key, value) in &self.map {
            match other.get(key) {
                Some(v) if v == value => {}
                _ => return false,
            }
        }
        true
    }
}

impl PartialEq for RepeatableStringMap {
    fn eq(&self, other: &Self) -> bool {
        self.equal(other)
    }
}

impl ParseCli for RepeatableStringMap {
    fn parse_cli(&mut self, input: Option<&str>) -> Result<(), Error> {
        let value = input.ok_or(Error::ValueRequired)?;

        // Empty value resets the list.
        if value.is_empty() {
            self.map.clear();
            return Ok(());
        }

        let index = value.find('=').ok_or(Error::ValueRequired)?;

        let key = value[..index].trim_matches(ASCII_WHITESPACE);
        let val = value[index + 1..].trim_matches(ASCII_WHITESPACE);

        // Empty value removes the key from the map.
        if val.is_empty() {
            self.ordered_remove(key);
            return Ok(());
        }

        self.put(StringZ::from(key), StringZ::from(val));
        Ok(())
    }

    fn parse_cli_new(input: Option<&str>) -> Result<Self, Error> {
        parse_cli_new_from_default(input)
    }
}

impl FormatEntry for RepeatableStringMap {
    /// Used by formatter
    fn format_entry(&self, formatter: &mut EntryFormatter<'_>) -> Result<(), formatter::Error> {
        // If no items, we want to render an empty field.
        if self.map.is_empty() {
            return formatter.format_entry(&());
        }

        for (key, value) in &self.map {
            write!(formatter.writer, "{} = {}={}\n", formatter.name, key, value)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::formatter::entry_formatter;

    #[test]
    fn repeatable_string_map_parse_cli() {
        let mut map = RepeatableStringMap::default();

        assert_eq!(Err(Error::ValueRequired), map.parse_cli(Some("A")));

        map.parse_cli(Some("A=B")).unwrap();
        map.parse_cli(Some("B=C")).unwrap();
        assert_eq!(2, map.count());

        map.parse_cli(Some("")).unwrap();
        assert_eq!(0, map.count());

        map.parse_cli(Some("A=B")).unwrap();
        assert_eq!(1, map.count());
        map.parse_cli(Some("A=C")).unwrap();
        assert_eq!(1, map.count());

        // Empty value removes the key.
        map.parse_cli(Some("A=")).unwrap();
        assert_eq!(0, map.count());
    }

    #[test]
    fn repeatable_string_map_format_config_empty() {
        let mut buf = String::new();
        let list = RepeatableStringMap::default();
        list.format_entry(&mut entry_formatter("a", &mut buf)).unwrap();
        assert_eq!("a = \n", buf);
    }

    #[test]
    fn repeatable_string_map_format_config_single_item() {
        {
            let mut buf = String::new();
            let mut map = RepeatableStringMap::default();
            map.parse_cli(Some("A=B")).unwrap();
            map.format_entry(&mut entry_formatter("a", &mut buf)).unwrap();
            assert_eq!("a = A=B\n", buf);
        }
        {
            let mut buf = String::new();
            let mut map = RepeatableStringMap::default();
            map.parse_cli(Some(" A = B ")).unwrap();
            map.format_entry(&mut entry_formatter("a", &mut buf)).unwrap();
            assert_eq!("a = A=B\n", buf);
        }
    }

    #[test]
    fn repeatable_string_map_format_config_multiple_items() {
        let mut buf = String::new();
        let mut list = RepeatableStringMap::default();
        list.parse_cli(Some("A=B")).unwrap();
        list.parse_cli(Some("B = C")).unwrap();
        list.format_entry(&mut entry_formatter("a", &mut buf)).unwrap();
        assert_eq!("a = A=B\na = B=C\n", buf);
    }

    #[test]
    fn repeatable_string_map_format_config_long_value() {
        let value = "x".repeat(300);
        let input = format!("EXAMPLE={}", value);

        let mut map = RepeatableStringMap::default();
        map.parse_cli(Some(&input)).unwrap();

        let mut output = String::new();
        map.format_entry(&mut entry_formatter("env", &mut output)).unwrap();

        let expected = format!("env = {}\n", input);
        assert_eq!(expected, output);
    }

    #[test]
    fn repeatable_string_map_clone_equal() {
        let mut map = RepeatableStringMap::default();
        map.parse_cli(Some("A=B")).unwrap();
        map.parse_cli(Some("C=D")).unwrap();
        let cloned = map.clone();
        assert!(map.equal(&cloned));

        let mut other = RepeatableStringMap::default();
        other.parse_cli(Some("C=D")).unwrap();
        other.parse_cli(Some("A=B")).unwrap();
        assert!(map.equal(&other));

        other.parse_cli(Some("A=X")).unwrap();
        assert!(!map.equal(&other));
    }
}
