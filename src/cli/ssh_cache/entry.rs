//! Port of `src/cli/ssh-cache/Entry.zig`.
//!
//! STATUS: ported
//!
//! A single entry within our SSH entry cache. Our SSH entry cache
//! stores which hosts we've sent our terminfo to so that we don't have
//! to send it again. It doesn't store any sensitive information.

use std::fmt;
use std::io;

use crate::lib::parse_int::std_parse_int;

/// A single entry within our SSH entry cache.
///
/// The Zig struct borrows its strings from the parsed line; the Rust port
/// owns them (the disk cache dupes them anyway).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub hostname: String,
    pub timestamp: i64,
    pub terminfo_version: String,
}

/// `std.Io.Writer.Error`.
pub type FormatError = io::Error;

impl Entry {
    pub fn parse(line: &str) -> Option<Entry> {
        let trimmed = line.trim_matches(|c| matches!(c, ' ' | '\t' | '\r' | '\n'));
        if trimmed.is_empty() {
            return None;
        }

        // Parse format: hostname|timestamp|terminfo_version
        // (tokenizeScalar: empty tokens are skipped)
        let mut iter = trimmed.split('|').filter(|s| !s.is_empty());
        let hostname = iter.next()?;
        let timestamp_str = iter.next()?;
        let terminfo_version = iter.next().unwrap_or("xterm-ghostty");
        let timestamp = match std_parse_int::<i64>(timestamp_str.as_bytes(), 10) {
            Ok(v) => v,
            Err(err) => {
                log::warn!(
                    "Invalid timestamp in cache entry: {} err={:?}",
                    timestamp_str,
                    err
                );
                return None;
            }
        };

        Some(Entry {
            hostname: hostname.to_owned(),
            timestamp,
            terminfo_version: terminfo_version.to_owned(),
        })
    }

    pub fn format<W: io::Write + ?Sized>(&self, writer: &mut W) -> Result<(), FormatError> {
        write!(
            writer,
            "{}|{}|{}\n",
            self.hostname, self.timestamp, self.terminfo_version
        )
    }
}

impl fmt::Display for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}|{}|{}\n",
            self.hostname, self.timestamp, self.terminfo_version
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_entry_parsing_valid_formats() {
        let entry = Entry::parse("example.com|1640995200|xterm-ghostty").unwrap();
        assert_eq!("example.com", entry.hostname);
        assert_eq!(1640995200i64, entry.timestamp);
        assert_eq!("xterm-ghostty", entry.terminfo_version);

        // Test default terminfo version
        let entry_no_version = Entry::parse("test.com|1640995200").unwrap();
        assert_eq!("xterm-ghostty", entry_no_version.terminfo_version);

        // Test complex hostnames
        let complex_entry =
            Entry::parse("user@server.example.com|1640995200|xterm-ghostty").unwrap();
        assert_eq!("user@server.example.com", complex_entry.hostname);
    }

    #[test]
    fn cache_entry_parsing_invalid_formats() {
        assert!(Entry::parse("").is_none());

        // Invalid format (no pipe)
        assert!(Entry::parse("v1").is_none());

        // Missing timestamp
        assert!(Entry::parse("example.com").is_none());

        // Invalid timestamp
        assert!(Entry::parse("example.com|invalid").is_none());

        // Empty terminfo should default
        assert!(Entry::parse("example.com|1640995200|").is_some());
    }

    #[test]
    fn cache_entry_parsing_malformed_data_resilience() {
        // Extra pipes should not break parsing
        assert!(Entry::parse("host|123|term|extra").is_some());

        // Whitespace handling
        assert!(Entry::parse("  host|123|term  ").is_some());
        assert!(Entry::parse("\n").is_none());
        assert!(Entry::parse("   \t  \n").is_none());

        // Extremely large timestamp
        assert!(Entry::parse(
            "host|999999999999999999999999999999999999999999999999|xterm-ghostty"
        )
        .is_none());
    }
}
