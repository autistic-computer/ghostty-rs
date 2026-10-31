//! Port of `src/terminal/selection_codepoints.zig`.
//!
//! STATUS: ported
//!
//! This file contains various default word boundaries used for
//! selection logic. We put it in a separate file so that different
//! subsystems can import it without introducing a number of
//! dependencies.

/// Default boundary characters for word selection: `` \t'"│`|:;,()[]{}<>$ ``
pub const DEFAULT_WORD_BOUNDARIES: [u32; 20] = [
    0,           // null
    ' ' as u32,  // space
    '\t' as u32, // tab
    '\'' as u32, // single quote
    '"' as u32,  // double quote
    '│' as u32,  // U+2502 box drawing
    '`' as u32,  // backtick
    '|' as u32,  // pipe
    ':' as u32,  // colon
    ';' as u32,  // semicolon
    ',' as u32,  // comma
    '(' as u32,  // left paren
    ')' as u32,  // right paren
    '[' as u32,  // left bracket
    ']' as u32,  // right bracket
    '{' as u32,  // left brace
    '}' as u32,  // right brace
    '<' as u32,  // less than
    '>' as u32,  // greater than
    '$' as u32,  // dollar
];

/// Default whitespace characters trimmed from line selections.
pub const DEFAULT_LINE_WHITESPACE: [u32; 3] = [0, ' ' as u32, '\t' as u32];
