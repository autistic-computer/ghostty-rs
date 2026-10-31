//! Port of `src/cli/CommaSplitter.zig`.
//!
//! STATUS: ported
//!
//! Iterator to split a string into fields by commas, taking into account
//! quotes and escapes.
//!
//! Supports the same escapes as in Zig literal strings.
//!
//! Quotes must begin and end with a double quote (`"`). It is an error to not
//! end a quote that was begun. To include a double quote inside a quote (or to
//! not have a double quote start a quoted section) escape it with a backslash.
//!
//! Single quotes (`'`) are not special, they do not begin a quoted block.
//!
//! Zig multiline string literals are NOT supported.
//!
//! Quotes and escapes are not stripped or decoded, that must be handled as a
//! separate step!
//!
//! On Windows, backslash is only treated as an escape character inside quoted
//! strings. Outside quotes, backslash is a literal character (path separator).

use std::fmt;

/// Whether backslash acts as an escape character outside quoted strings.
/// On Windows, backslash is the path separator so it is always literal
/// outside quotes.
const ESCAPE_OUTSIDE_QUOTES: bool = !cfg!(windows);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    UnclosedQuote,
    UnfinishedEscape,
    IllegalEscape,
}

impl Error {
    /// The Zig error name.
    pub fn name(self) -> &'static str {
        match self {
            Error::UnclosedQuote => "UnclosedQuote",
            Error::UnfinishedEscape => "UnfinishedEscape",
            Error::IllegalEscape => "IllegalEscape",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "error.{}", self.name())
    }
}

impl std::error::Error for Error {}

/// State of the state machine in [`CommaSplitter::next`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Normal,
    Quoted,
    Escape,
    Hexescape,
    Unicodeescape,
}

/// Sub-state of parsing unicode escapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnicodeEscapeState {
    Start,
    Digits,
}

#[derive(Debug, Clone)]
pub struct CommaSplitter<'a> {
    /// the string that we are splitting
    pub str: &'a str,
    /// how much of the string has been consumed so far
    pub index: usize,
}

impl<'a> CommaSplitter<'a> {
    /// initialize a splitter with the given string
    pub fn init(str: &'a str) -> CommaSplitter<'a> {
        CommaSplitter { str, index: 0 }
    }

    /// return the next field, null if no more fields
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Option<&'a str>, Error> {
        let bytes = self.str.as_bytes();
        if self.index >= bytes.len() {
            return Ok(None);
        }

        // where the current field starts
        let start = self.index;
        // keep track of the state to return to when done processing an escape
        // sequence.
        let mut last = State::Normal;
        // used to count number of digits seen in a hex escape
        let mut hexescape_digits: usize = 0;
        // sub-state of parsing hex escapes
        let mut unicodeescape_state = UnicodeEscapeState::Start;
        // number of digits in a unicode escape seen so far
        let mut unicodeescape_digits: usize = 0;
        // accumulator for value of unicode escape
        let mut unicodeescape_value: usize = 0;

        let mut state = State::Normal;
        loop {
            match state {
                State::Normal => {
                    if self.index >= bytes.len() {
                        return Ok(Some(&self.str[start..]));
                    }
                    match bytes[self.index] {
                        b',' => {
                            self.index += 1;
                            return Ok(Some(&self.str[start..self.index - 1]));
                        }
                        b'"' => {
                            self.index += 1;
                            state = State::Quoted;
                        }
                        b'\\' => {
                            self.index += 1;
                            if ESCAPE_OUTSIDE_QUOTES {
                                last = State::Normal;
                                state = State::Escape;
                            } else {
                                state = State::Normal;
                            }
                        }
                        _ => {
                            self.index += 1;
                            state = State::Normal;
                        }
                    }
                }
                State::Quoted => {
                    if self.index >= bytes.len() {
                        return Err(Error::UnclosedQuote);
                    }
                    match bytes[self.index] {
                        b'"' => {
                            self.index += 1;
                            state = State::Normal;
                        }
                        b'\\' => {
                            self.index += 1;
                            last = State::Quoted;
                            state = State::Escape;
                        }
                        _ => {
                            self.index += 1;
                            state = State::Quoted;
                        }
                    }
                }
                State::Escape => {
                    if self.index >= bytes.len() {
                        return Err(Error::UnfinishedEscape);
                    }
                    match bytes[self.index] {
                        b'n' | b'r' | b't' | b'\\' | b'\'' | b'"' => {
                            self.index += 1;
                            state = last;
                        }
                        b'x' => {
                            self.index += 1;
                            hexescape_digits = 0;
                            state = State::Hexescape;
                        }
                        b'u' => {
                            self.index += 1;
                            unicodeescape_state = UnicodeEscapeState::Start;
                            unicodeescape_digits = 0;
                            unicodeescape_value = 0;
                            state = State::Unicodeescape;
                        }
                        _ => return Err(Error::IllegalEscape),
                    }
                }
                State::Hexescape => {
                    if self.index >= bytes.len() {
                        return Err(Error::UnfinishedEscape);
                    }
                    match bytes[self.index] {
                        b'0'..=b'9' | b'a'..=b'f' | b'A'..=b'F' => {
                            self.index += 1;
                            hexescape_digits += 1;
                            if hexescape_digits == 2 {
                                state = last;
                            } else {
                                state = State::Hexescape;
                            }
                        }
                        _ => return Err(Error::IllegalEscape),
                    }
                }
                State::Unicodeescape => {
                    if self.index >= bytes.len() {
                        return Err(Error::UnfinishedEscape);
                    }
                    match unicodeescape_state {
                        UnicodeEscapeState::Start => match bytes[self.index] {
                            b'{' => {
                                self.index += 1;
                                unicodeescape_value = 0;
                                unicodeescape_state = UnicodeEscapeState::Digits;
                                state = State::Unicodeescape;
                            }
                            _ => return Err(Error::IllegalEscape),
                        },
                        UnicodeEscapeState::Digits => {
                            match bytes[self.index] {
                                b'}' => {
                                    self.index += 1;
                                    if unicodeescape_digits == 0 {
                                        return Err(Error::IllegalEscape);
                                    }
                                    state = last;
                                    continue;
                                }
                                d @ b'0'..=b'9' => {
                                    self.index += 1;
                                    unicodeescape_digits += 1;
                                    unicodeescape_value <<= 4;
                                    unicodeescape_value += (d - b'0') as usize;
                                }
                                d @ b'a'..=b'f' => {
                                    self.index += 1;
                                    unicodeescape_digits += 1;
                                    unicodeescape_value <<= 4;
                                    unicodeescape_value += (d - b'a' + 10) as usize;
                                }
                                d @ b'A'..=b'F' => {
                                    self.index += 1;
                                    unicodeescape_digits += 1;
                                    unicodeescape_value <<= 4;
                                    unicodeescape_value += (d - b'A' + 10) as usize;
                                }
                                _ => return Err(Error::IllegalEscape),
                            }
                            if unicodeescape_value > 0x10ffff {
                                return Err(Error::IllegalEscape);
                            }
                            state = State::Unicodeescape;
                        }
                    }
                }
            }
        }
    }

    /// Return any remaining string data, whether it has a comma or not.
    pub fn rest(&mut self) -> Option<&'a str> {
        if self.index >= self.str.len() {
            return None;
        }
        let result = &self.str[self.index..];
        self.index = self.str.len();
        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitter_1() {
        let mut s = CommaSplitter::init("a,b,c");
        assert_eq!("a", s.next().unwrap().unwrap());
        assert_eq!("b", s.next().unwrap().unwrap());
        assert_eq!("c", s.next().unwrap().unwrap());
        assert_eq!(None, s.next().unwrap());
    }

    #[test]
    fn splitter_2() {
        let mut s = CommaSplitter::init("");
        assert_eq!(None, s.next().unwrap());
    }

    #[test]
    fn splitter_3() {
        let mut s = CommaSplitter::init("a");
        assert_eq!("a", s.next().unwrap().unwrap());
        assert_eq!(None, s.next().unwrap());
    }

    #[test]
    fn splitter_4() {
        let mut s = CommaSplitter::init("\\x5a");
        assert_eq!("\\x5a", s.next().unwrap().unwrap());
        assert_eq!(None, s.next().unwrap());
    }

    #[test]
    fn splitter_5() {
        let mut s = CommaSplitter::init("'a',b");
        assert_eq!("'a'", s.next().unwrap().unwrap());
        assert_eq!("b", s.next().unwrap().unwrap());
        assert_eq!(None, s.next().unwrap());
    }

    #[test]
    fn splitter_6() {
        let mut s = CommaSplitter::init("'a,b',c");
        assert_eq!("'a", s.next().unwrap().unwrap());
        assert_eq!("b'", s.next().unwrap().unwrap());
        assert_eq!("c", s.next().unwrap().unwrap());
        assert_eq!(None, s.next().unwrap());
    }

    #[test]
    fn splitter_7() {
        let mut s = CommaSplitter::init("\"a,b\",c");
        assert_eq!("\"a,b\"", s.next().unwrap().unwrap());
        assert_eq!("c", s.next().unwrap().unwrap());
        assert_eq!(None, s.next().unwrap());
    }

    #[test]
    fn splitter_8() {
        let mut s = CommaSplitter::init(" a , b ");
        assert_eq!(" a ", s.next().unwrap().unwrap());
        assert_eq!(" b ", s.next().unwrap().unwrap());
        assert_eq!(None, s.next().unwrap());
    }

    #[test]
    #[cfg(not(windows))]
    fn splitter_9() {
        let mut s = CommaSplitter::init("\\x");
        assert_eq!(Err(Error::UnfinishedEscape), s.next());
    }

    #[test]
    #[cfg(not(windows))]
    fn splitter_10() {
        let mut s = CommaSplitter::init("\\x5");
        assert_eq!(Err(Error::UnfinishedEscape), s.next());
    }

    #[test]
    #[cfg(not(windows))]
    fn splitter_11() {
        let mut s = CommaSplitter::init("\\u");
        assert_eq!(Err(Error::UnfinishedEscape), s.next());
    }

    #[test]
    #[cfg(not(windows))]
    fn splitter_12() {
        let mut s = CommaSplitter::init("\\u{");
        assert_eq!(Err(Error::UnfinishedEscape), s.next());
    }

    #[test]
    #[cfg(not(windows))]
    fn splitter_13() {
        let mut s = CommaSplitter::init("\\u{}");
        assert_eq!(Err(Error::IllegalEscape), s.next());
    }

    #[test]
    #[cfg(not(windows))]
    fn splitter_14() {
        let mut s = CommaSplitter::init("\\u{h1}");
        assert_eq!(Err(Error::IllegalEscape), s.next());
    }

    #[test]
    fn splitter_15() {
        let mut s = CommaSplitter::init("\\u{10ffff}");
        assert_eq!("\\u{10ffff}", s.next().unwrap().unwrap());
        assert_eq!(None, s.next().unwrap());
    }

    #[test]
    #[cfg(not(windows))]
    fn splitter_16() {
        let mut s = CommaSplitter::init("\\u{110000}");
        assert_eq!(Err(Error::IllegalEscape), s.next());
    }

    #[test]
    #[cfg(not(windows))]
    fn splitter_17() {
        let mut s = CommaSplitter::init("\\d");
        assert_eq!(Err(Error::IllegalEscape), s.next());
    }

    #[test]
    #[cfg(not(windows))]
    fn splitter_18() {
        let mut s = CommaSplitter::init("\\n\\r\\t\\\"\\'\\\\");
        assert_eq!("\\n\\r\\t\\\"\\'\\\\", s.next().unwrap().unwrap());
        assert_eq!(None, s.next().unwrap());
    }

    #[test]
    fn splitter_19() {
        let mut s = CommaSplitter::init("\"abc'def'ghi\"");
        assert_eq!("\"abc'def'ghi\"", s.next().unwrap().unwrap());
        assert_eq!(None, s.next().unwrap());
    }

    #[test]
    fn splitter_20() {
        let mut s = CommaSplitter::init("\",\",abc");
        assert_eq!("\",\"", s.next().unwrap().unwrap());
        assert_eq!("abc", s.next().unwrap().unwrap());
        assert_eq!(None, s.next().unwrap());
    }

    #[test]
    fn splitter_21() {
        let mut s = CommaSplitter::init("'a','b', 'c'");
        assert_eq!("'a'", s.next().unwrap().unwrap());
        assert_eq!("'b'", s.next().unwrap().unwrap());
        assert_eq!(" 'c'", s.next().unwrap().unwrap());
        assert_eq!(None, s.next().unwrap());
    }

    #[test]
    fn splitter_22() {
        let mut s = CommaSplitter::init("abc\"def");
        assert_eq!(Err(Error::UnclosedQuote), s.next());
    }

    #[test]
    fn splitter_23() {
        let mut s = CommaSplitter::init("title:\"Focus Split: Up\",description:\"Focus the split above, if it exists.\",action:goto_split:up");
        assert_eq!("title:\"Focus Split: Up\"", s.next().unwrap().unwrap());
        assert_eq!(
            "description:\"Focus the split above, if it exists.\"",
            s.next().unwrap().unwrap()
        );
        assert_eq!("action:goto_split:up", s.next().unwrap().unwrap());
        assert_eq!(None, s.next().unwrap());
    }

    #[test]
    fn splitter_24() {
        let mut s = CommaSplitter::init("a,b,c,def");
        assert_eq!("a", s.next().unwrap().unwrap());
        assert_eq!("b", s.next().unwrap().unwrap());
        assert_eq!("c,def", s.rest().unwrap());
        assert_eq!(None, s.next().unwrap());
    }

    #[test]
    #[cfg(not(windows))]
    fn splitter_25() {
        let mut s = CommaSplitter::init("a,\\u{10,df}");
        assert_eq!("a", s.next().unwrap().unwrap());
        assert_eq!(Err(Error::IllegalEscape), s.next());
    }

    #[test]
    #[cfg(not(windows))]
    fn splitter_rejects_oversized_unicode_escape_with_letters() {
        let mut s = CommaSplitter::init("\\u{aaaaaa}");
        assert_eq!(Err(Error::IllegalEscape), s.next());
    }

    #[test]
    #[cfg(not(windows))]
    fn splitter_rejects_oversized_unicode_escape_with_uppercase_letters() {
        let mut s = CommaSplitter::init("\\u{AAAAAA}");
        assert_eq!(Err(Error::IllegalEscape), s.next());
    }

    #[test]
    fn splitter_rejects_oversized_quoted_unicode_escape_with_letters() {
        let mut s = CommaSplitter::init("\"\\u{aaaaaa}\"");
        assert_eq!(Err(Error::IllegalEscape), s.next());
    }

    #[test]
    fn splitter_rejects_oversized_quoted_unicode_escape_with_uppercase_letters() {
        let mut s = CommaSplitter::init("\"\\u{AAAAAA}\"");
        assert_eq!(Err(Error::IllegalEscape), s.next());
    }

    // Windows-specific tests: backslash is literal outside quotes.

    #[test]
    #[cfg(windows)]
    fn splitter_windows_paths() {
        let mut s =
            CommaSplitter::init("light:C:\\Users\\foo\\theme,dark:C:\\Users\\bar\\theme");
        assert_eq!("light:C:\\Users\\foo\\theme", s.next().unwrap().unwrap());
        assert_eq!("dark:C:\\Users\\bar\\theme", s.next().unwrap().unwrap());
        assert_eq!(None, s.next().unwrap());
    }

    #[test]
    #[cfg(windows)]
    fn splitter_backslash_literal_outside_quotes_on_windows() {
        // Backslash followed by characters that would be escapes on Unix
        // are treated as literal on Windows outside quotes.
        let mut s = CommaSplitter::init("\\n\\r\\t");
        assert_eq!("\\n\\r\\t", s.next().unwrap().unwrap());
        assert_eq!(None, s.next().unwrap());
    }

    #[test]
    #[cfg(windows)]
    fn splitter_backslash_still_escapes_inside_quotes_on_windows() {
        // Inside quotes, backslash escapes work on all platforms.
        let mut s = CommaSplitter::init("\"hello\\nworld\"");
        assert_eq!("\"hello\\nworld\"", s.next().unwrap().unwrap());
        assert_eq!(None, s.next().unwrap());
    }
}
