//! Port of `src/terminal/StringMap.zig`.
//!
//! STATUS: ported
//!
//! A string along with the mapping of each individual byte in the string
//! to the point in the screen.
//!
//! Notes on the port:
//! - `string` is a `String` (the formatter emits UTF-8); `deinit` is a drop.
//! - The search iterator and matches borrow the map and the regex instead
//!   of copying them.

use crate::pkg::oniguruma as oni;
use crate::terminal::build_options;
use crate::terminal::formatter::PinMapMap;
use crate::terminal::selection::Selection;

// Retry budget for StringMap regex searches.
//
// Units are Oniguruma retry steps (internal backtracking/retry counter),
// not bytes/characters/time.
const ONI_SEARCH_RETRY_LIMIT: usize = 100_000;

#[derive(Debug, Clone, Default)]
pub struct StringMap {
    pub string: String,

    /// Mapping of string byte offsets to pins. See PinMap for the
    /// storage details.
    pub map: PinMapMap,
}

impl StringMap {
    pub fn deinit(self) {}

    /// Returns an iterator that yields the next match of the given regex.
    /// Requires Ghostty to be compiled with regex support.
    pub fn search_iterator<'a>(&'a self, regex: &'a mut oni::Regex) -> SearchIterator<'a> {
        debug_assert!(build_options::ONIGURUMA);
        SearchIterator {
            map: self,
            regex,
            offset: 0,
        }
    }
}

/// Iterates over the regular expression matches of the string.
pub struct SearchIterator<'a> {
    pub map: &'a StringMap,
    pub regex: &'a mut oni::Regex,
    pub offset: usize,
}

impl<'a> SearchIterator<'a> {
    /// Returns the next regular expression match or null if there are
    /// no more matches.
    pub fn next(&mut self) -> Result<Option<Match<'a>>, oni::errors::Error> {
        if self.offset >= self.map.string.len() {
            return Ok(None);
        }

        // Use per-search match params so we can bound regex retry steps
        // (Oniguruma's internal backtracking work counter).
        let mut match_param = oni::MatchParam::init()?;
        match_param.set_retry_limit_in_search(ONI_SEARCH_RETRY_LIMIT)?;

        let region = match self.regex.search_with_param(
            &self.map.string.as_bytes()[self.offset..],
            oni::types::Option::default(),
            Some(&mut match_param),
        ) {
            Ok(r) => r,
            // Retry/stack-limit errors mean we hit our work budget and
            // aborted matching.
            // For iterator callers this is equivalent to "no further matches".
            Err(
                oni::errors::Error::Mismatch
                | oni::errors::Error::RetryLimitInMatchOver
                | oni::errors::Error::RetryLimitInSearchOver
                | oni::errors::Error::MatchStackLimitOver
                | oni::errors::Error::SubexpCallLimitInSearchOver,
            ) => {
                self.offset = self.map.string.len();
                return Ok(None);
            }

            Err(err) => return Err(err),
        };

        // Increment our offset by the number of bytes in the match.
        // We defer this so that we can return the match before
        // modifying the offset.
        let end_idx: usize = region.ends()[0] as usize;
        let result = Match {
            map: self.map,
            offset: self.offset,
            region,
        };
        self.offset += end_idx;
        Ok(Some(result))
    }
}

/// A single regular expression match.
pub struct Match<'a> {
    pub map: &'a StringMap,
    pub offset: usize,
    pub region: oni::Region,
}

impl Match<'_> {
    pub fn deinit(&mut self) {
        self.region.deinit();
    }

    /// Returns the selection containing the full match.
    pub fn selection(&self) -> Selection {
        let start_idx: usize = self.region.starts()[0] as usize;
        let end_idx: usize = (self.region.ends()[0] - 1) as usize;
        let start_pt = self.map.map.get(self.offset + start_idx).unwrap();
        let end_pt = self.map.map.get(self.offset + end_idx).unwrap();
        Selection::init(start_pt, end_pt, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::point::{Coordinate, Point, Tag};
    use crate::terminal::screen::{Options, SelectLine, SelectionString, Screen};

    fn screen(cols: u16, rows: u16) -> Screen {
        Screen::init(Options {
            cols,
            rows,
            max_scrollback_bytes: Some(0),
            ..Default::default()
        })
        .unwrap()
    }

    fn active(x: u16, y: u32) -> Point {
        Point::Active(Coordinate { x, y })
    }

    fn scr(x: u16, y: u32) -> Point {
        Point::Screen(Coordinate { x, y })
    }

    #[test]
    fn string_map_search_iterator() {
        if !build_options::ONIGURUMA {
            return;
        }

        // Initialize our regex
        oni::testing::ensure_init().unwrap();
        let mut re = oni::Regex::init(
            b"[A-B]{2}",
            oni::types::Option::default(),
            oni::Encoding::utf8(),
            oni::Syntax::default(),
            None,
        )
        .unwrap();

        // Initialize our screen
        let mut s = screen(5, 5);
        let str_ = "1ABCD2EFGH\n3IJKL";
        s.test_write_string(str_).unwrap();
        let line = s.select_line(SelectLine::new(s.pages.pin(active(2, 1)).unwrap())).unwrap();
        let map = s.selection_string_map(SelectionString { sel: line, trim: false });

        // Get our iterator
        let mut it = map.search_iterator(&mut re);
        {
            let mut m = it.next().unwrap().unwrap();

            let sel = m.selection();
            assert_eq!(scr(1, 0), s.pages.point_from_pin(Tag::Screen, sel.start()).unwrap());
            assert_eq!(scr(2, 0), s.pages.point_from_pin(Tag::Screen, sel.end()).unwrap());
            m.deinit();
        }

        assert!(it.next().unwrap().is_none());
    }

    #[test]
    fn string_map_search_iterator_url_detection() {
        if !build_options::ONIGURUMA {
            return;
        }
        use crate::config::url;

        // Initialize URL regex
        oni::testing::ensure_init().unwrap();
        let mut re = oni::Regex::init(
            url::REGEX.as_bytes(),
            oni::types::Option::default(),
            oni::Encoding::utf8(),
            oni::Syntax::default(),
            None,
        )
        .unwrap();

        // Initialize our screen with text containing a URL
        let mut s = screen(40, 5);
        s.test_write_string("hello https://example.com/path world").unwrap();

        // Get the line
        let line = s.select_line(SelectLine::new(s.pages.pin(active(10, 0)).unwrap())).unwrap();
        let map = s.selection_string_map(SelectionString { sel: line, trim: false });

        // Search for URL match
        let mut it = map.search_iterator(&mut re);
        {
            let mut m = it.next().unwrap().unwrap();

            let sel = m.selection();
            // URL should start at x=6 ("https://example.com/path" starts after "hello ")
            assert_eq!(scr(6, 0), s.pages.point_from_pin(Tag::Screen, sel.start()).unwrap());
            // URL should end at x=29 (end of "/path")
            assert_eq!(scr(29, 0), s.pages.point_from_pin(Tag::Screen, sel.end()).unwrap());
            m.deinit();
        }

        assert!(it.next().unwrap().is_none());
    }

    #[test]
    fn string_map_search_iterator_url_with_click_position() {
        if !build_options::ONIGURUMA {
            return;
        }
        use crate::config::url;

        // Initialize URL regex
        oni::testing::ensure_init().unwrap();
        let mut re = oni::Regex::init(
            url::REGEX.as_bytes(),
            oni::types::Option::default(),
            oni::Encoding::utf8(),
            oni::Syntax::default(),
            None,
        )
        .unwrap();

        // Initialize our screen with text containing a URL
        let mut s = screen(40, 5);
        s.test_write_string("hello https://example.com world").unwrap();

        // Simulate clicking on "example" (x=14)
        let click_pin = s.pages.pin(active(14, 0)).unwrap();

        // Get the line
        let line = s.select_line(SelectLine::new(click_pin)).unwrap();
        let map = s.selection_string_map(SelectionString { sel: line, trim: false });

        // Search for URL match and verify click position is within URL
        let mut it = map.search_iterator(&mut re);
        let mut found_url = false;
        while let Some(mut m) = it.next().unwrap() {
            let sel = m.selection();
            if sel.contains(&s, click_pin) {
                found_url = true;
                // Verify URL bounds
                assert_eq!(scr(6, 0), s.pages.point_from_pin(Tag::Screen, sel.start()).unwrap());
                assert_eq!(scr(24, 0), s.pages.point_from_pin(Tag::Screen, sel.end()).unwrap());
                m.deinit();
                break;
            }
            m.deinit();
        }
        assert!(found_url);
    }
}
