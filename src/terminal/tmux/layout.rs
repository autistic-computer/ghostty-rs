//! Port of `src/terminal/tmux/layout.zig`.
//!
//! STATUS: ported
//!
//! Deviation: Zig allocates the tree into a caller-provided (arena)
//! allocator; here children are owned `Vec`s and the allocation error
//! variant is dropped.

use std::fmt;

use super::output::parse_int_usize;

/// A tmux layout.
///
/// This is a tree structure so by definition it pretty much needs to be
/// allocated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// The width and height of this node, measured in terminal cells.
    ///
    /// Parsing guarantees both values are at least 1. They are not
    /// bounded above, so check that they fit your own size type before
    /// using them (see `size.CellCountInt` for terminals).
    pub width: usize,
    pub height: usize,

    /// X and Y offset from the top-left corner of the window.
    pub x: usize,
    pub y: usize,

    /// The content of this node, either a pane (leaf) or more nodes
    /// (split) horizontally or vertically.
    pub content: Content,
}

/// `Layout.Content`
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    Pane(usize),
    Horizontal(Vec<Layout>),
    Vertical(Vec<Layout>),
}

impl Content {
    /// The pane ID, panicking if this is not a pane (Zig `content.pane`).
    pub fn pane(&self) -> usize {
        match self {
            Content::Pane(id) => *id,
            _ => panic!("access of union field 'pane' while field is not active"),
        }
    }

    /// The children of a horizontal split (Zig `content.horizontal`).
    pub fn horizontal(&self) -> &[Layout] {
        match self {
            Content::Horizontal(v) => v,
            _ => panic!("access of union field 'horizontal' while field is not active"),
        }
    }

    /// The children of a vertical split (Zig `content.vertical`).
    pub fn vertical(&self) -> &[Layout] {
        match self {
            Content::Vertical(v) => v,
            _ => panic!("access of union field 'vertical' while field is not active"),
        }
    }
}

/// Errors returned by `parse` and `parse_with_checksum`.
///
/// `SyntaxError` means the string is not a valid layout. This includes
/// strings that are well formed but describe a node with a width or
/// height of zero. `ChecksumMismatch` is only returned by
/// `parse_with_checksum`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    SyntaxError,
    ChecksumMismatch,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for ParseError {}

fn index_of_scalar(s: &[u8], c: u8) -> Option<usize> {
    s.iter().position(|&b| b == c)
}

fn index_of_any(s: &[u8], set: &[u8]) -> Option<usize> {
    s.iter().position(|b| set.contains(b))
}

fn parse_num(s: &[u8]) -> Result<usize, ParseError> {
    parse_int_usize(s).map_err(|_| ParseError::SyntaxError)
}

impl Layout {
    /// Parse a layout string that starts with a 4-character checksum.
    ///
    /// This is the form tmux sends in `%layout-change` notifications and
    /// in `list-windows` output. The string is the checksum in lowercase
    /// hexadecimal, a comma, and then the layout itself:
    ///
    /// ```ignore
    /// let layout = Layout::parse_with_checksum(
    ///     b"f8f9,80x24,0,0{40x24,0,0,1,40x24,40,0,2}",
    /// )?;
    /// ```
    ///
    /// The checksum only catches accidental corruption. Anyone can compute
    /// it, so a matching checksum does not mean the layout is trustworthy.
    /// The layout is validated by `parse` either way.
    ///
    /// Returns `ChecksumMismatch` if the checksum doesn't match the layout.
    /// Returns `SyntaxError` if the layout is invalid. See `parse` for
    /// what counts as invalid.
    pub fn parse_with_checksum(str: &[u8]) -> Result<Layout, ParseError> {
        // If the string is less than 5 characters, it can't possibly
        // be correct. 4-char checksum + comma. In practice it should
        // be even longer, but that'll fail parse later.
        if str.len() < 5 {
            return Err(ParseError::SyntaxError);
        }
        if str[4] != b',' {
            return Err(ParseError::SyntaxError);
        }

        // The layout string should start with a 4-character checksum.
        let checksum = Checksum::calculate(&str[5..]);
        if !str.starts_with(&checksum.as_string()) {
            return Err(ParseError::ChecksumMismatch);
        }

        // Checksum matches, parse the rest.
        Self::parse(&str[5..])
    }

    /// Parse a layout string (without a checksum) into a `Layout` tree.
    ///
    /// The layout must be passed as one complete string. Layouts
    /// are small, so this is rarely a problem.
    ///
    /// Each node starts with its size and position, followed by either a
    /// pane ID or a list of child nodes:
    ///
    /// - `WxH,X,Y,ID` is a pane that is W cells wide and H cells tall,
    ///   placed at column X and row Y, with tmux pane ID `ID`.
    /// - `WxH,X,Y{...}` splits the area left to right. The children are
    ///   separated by commas.
    /// - `WxH,X,Y[...]` splits the area top to bottom. The children are
    ///   separated by commas.
    ///
    /// For example, this is an 80x24 window split into two side-by-side
    /// panes with IDs 1 and 2:
    ///
    /// ```ignore
    /// let layout = Layout::parse(b"80x24,0,0{40x24,0,0,1,40x24,40,0,2}")?;
    /// // layout.width == 80, layout.height == 24
    /// // layout.content.horizontal()[1].content.pane() == 2
    /// ```
    ///
    /// Returns `SyntaxError` if the string is malformed, has trailing
    /// characters, or contains any node with a width or height of zero.
    /// tmux never produces zero-sized nodes, so such a string can only
    /// come from a buggy or malicious sender.
    pub fn parse(str: &[u8]) -> Result<Layout, ParseError> {
        let mut offset: usize = 0;
        let root = Self::parse_next(str, &mut offset)?;
        if offset != str.len() {
            return Err(ParseError::SyntaxError);
        }
        Ok(root)
    }

    fn parse_next(str: &[u8], offset: &mut usize) -> Result<Layout, ParseError> {
        // Find the first `x` to grab the width.
        let width = match index_of_scalar(&str[*offset..], b'x') {
            Some(idx) => {
                let v = parse_num(&str[*offset..*offset + idx]);
                *offset += idx + 1; // Consume `x`
                v?
            }
            None => return Err(ParseError::SyntaxError),
        };

        // Find the height, up to a comma.
        let height = match index_of_scalar(&str[*offset..], b',') {
            Some(idx) => {
                let v = parse_num(&str[*offset..*offset + idx]);
                *offset += idx + 1; // Consume `,`
                v?
            }
            None => return Err(ParseError::SyntaxError),
        };

        // tmux never produces zero-sized nodes, and a terminal can't
        // have zero rows or columns, so reject them here.
        if width == 0 || height == 0 {
            return Err(ParseError::SyntaxError);
        }

        // Find X
        let x = match index_of_scalar(&str[*offset..], b',') {
            Some(idx) => {
                let v = parse_num(&str[*offset..*offset + idx]);
                *offset += idx + 1; // Consume `,`
                v?
            }
            None => return Err(ParseError::SyntaxError),
        };

        // Find Y, which can end in any of `,{,[`
        let y = match index_of_any(&str[*offset..], b",{[") {
            Some(idx) => {
                let v = parse_num(&str[*offset..*offset + idx]);
                *offset += idx; // Don't consume the delimiter!
                v?
            }
            None => return Err(ParseError::SyntaxError),
        };

        // Determine our child node.
        let content = match str[*offset] {
            b',' => {
                // Consume the delimiter
                *offset += 1;

                // Leaf pane. Read up to `,}]` because we may be in
                // a set of nodes. If none exist, end of string is fine.
                let idx = index_of_any(&str[*offset..], b",}]").unwrap_or(str.len() - *offset);

                let pane_id = parse_num(&str[*offset..*offset + idx]);
                *offset += idx; // Consume the pane ID, not the delimiter
                Content::Pane(pane_id?)
            }

            opening @ (b'{' | b'[') => {
                let mut nodes: Vec<Layout> = Vec::new();

                // Move beyond our opening
                *offset += 1;

                loop {
                    nodes.push(Self::parse_next(str, offset)?);

                    // We should not reach the end of string here because
                    // we expect a closing bracket.
                    if *offset >= str.len() {
                        return Err(ParseError::SyntaxError);
                    }

                    // If it is a comma, we expect another node.
                    if str[*offset] == b',' {
                        *offset += 1; // Consume
                        continue;
                    }

                    // We expect a closing bracket now.
                    match opening {
                        b'{' => {
                            if str[*offset] != b'}' {
                                return Err(ParseError::SyntaxError);
                            }
                        }
                        b'[' => {
                            if str[*offset] != b']' {
                                return Err(ParseError::SyntaxError);
                            }
                        }
                        _ => return Err(ParseError::SyntaxError),
                    }

                    // Successfully parsed all children.
                    *offset += 1; // Consume closing bracket
                    break match opening {
                        b'{' => Content::Horizontal(nodes),
                        b'[' => Content::Vertical(nodes),
                        _ => unreachable!(),
                    };
                }
            }

            // index_of_any above guarantees we have only the above
            _ => unreachable!(),
        };

        Ok(Layout {
            width,
            height,
            x,
            y,
            content,
        })
    }
}

/// A tmux layout checksum. Non-exhaustive `enum(u16)` in Zig.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct Checksum(pub u16);

impl Checksum {
    /// Calculate the checksum of a tmux layout string.
    /// The algorithm rotates the checksum right by 1 bit (with wraparound)
    /// and adds the ASCII value of each character.
    pub fn calculate(str: &[u8]) -> Checksum {
        let mut result: u16 = 0;
        for &c in str {
            // Rotate right by 1: (result >> 1) + ((result & 1) << 15)
            result = (result >> 1) | ((result & 1) << 15);
            result = result.wrapping_add(c as u16);
        }

        Checksum(result)
    }

    /// Convert the checksum to a 4-character hexadecimal string. This
    /// is always zero-padded to match the tmux implementation
    /// (in layout-custom.c).
    pub fn as_string(self) -> [u8; 4] {
        let value = self.0;
        const CHARSET: &[u8; 16] = b"0123456789abcdef";
        [
            CHARSET[((value >> 12) & 0xf) as usize],
            CHARSET[((value >> 8) & 0xf) as usize],
            CHARSET[((value >> 4) & 0xf) as usize],
            CHARSET[(value & 0xf) as usize],
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(s: &[u8]) -> ParseError {
        Layout::parse(s).unwrap_err()
    }

    #[test]
    fn simple_single_pane() {
        let layout = Layout::parse(b"80x24,0,0,42").unwrap();
        assert_eq!(80, layout.width);
        assert_eq!(24, layout.height);
        assert_eq!(0, layout.x);
        assert_eq!(0, layout.y);
        assert_eq!(42, layout.content.pane());
    }

    #[test]
    fn single_pane_with_offset() {
        let layout = Layout::parse(b"40x12,10,5,7").unwrap();
        assert_eq!(40, layout.width);
        assert_eq!(12, layout.height);
        assert_eq!(10, layout.x);
        assert_eq!(5, layout.y);
        assert_eq!(7, layout.content.pane());
    }

    #[test]
    fn single_pane_large_values() {
        let layout = Layout::parse(b"1920x1080,100,200,999").unwrap();
        assert_eq!(1920, layout.width);
        assert_eq!(1080, layout.height);
        assert_eq!(100, layout.x);
        assert_eq!(200, layout.y);
        assert_eq!(999, layout.content.pane());
    }

    #[test]
    fn horizontal_split_two_panes() {
        let layout = Layout::parse(b"80x24,0,0{40x24,0,0,1,40x24,40,0,2}").unwrap();
        assert_eq!(80, layout.width);
        assert_eq!(24, layout.height);
        assert_eq!(0, layout.x);
        assert_eq!(0, layout.y);

        let children = layout.content.horizontal();
        assert_eq!(2, children.len());

        assert_eq!(40, children[0].width);
        assert_eq!(24, children[0].height);
        assert_eq!(0, children[0].x);
        assert_eq!(0, children[0].y);
        assert_eq!(1, children[0].content.pane());

        assert_eq!(40, children[1].width);
        assert_eq!(24, children[1].height);
        assert_eq!(40, children[1].x);
        assert_eq!(0, children[1].y);
        assert_eq!(2, children[1].content.pane());
    }

    #[test]
    fn vertical_split_two_panes() {
        let layout = Layout::parse(b"80x24,0,0[80x12,0,0,1,80x12,0,12,2]").unwrap();
        assert_eq!(80, layout.width);
        assert_eq!(24, layout.height);
        assert_eq!(0, layout.x);
        assert_eq!(0, layout.y);

        let children = layout.content.vertical();
        assert_eq!(2, children.len());

        assert_eq!(80, children[0].width);
        assert_eq!(12, children[0].height);
        assert_eq!(0, children[0].x);
        assert_eq!(0, children[0].y);
        assert_eq!(1, children[0].content.pane());

        assert_eq!(80, children[1].width);
        assert_eq!(12, children[1].height);
        assert_eq!(0, children[1].x);
        assert_eq!(12, children[1].y);
        assert_eq!(2, children[1].content.pane());
    }

    #[test]
    fn horizontal_split_three_panes() {
        let layout =
            Layout::parse(b"120x24,0,0{40x24,0,0,1,40x24,40,0,2,40x24,80,0,3}").unwrap();
        assert_eq!(120, layout.width);
        assert_eq!(24, layout.height);

        let children = layout.content.horizontal();
        assert_eq!(3, children.len());
        assert_eq!(1, children[0].content.pane());
        assert_eq!(2, children[1].content.pane());
        assert_eq!(3, children[2].content.pane());
    }

    #[test]
    fn nested_horizontal_in_vertical() {
        // Vertical split with top pane and bottom horizontal split
        let layout =
            Layout::parse(b"80x24,0,0[80x12,0,0,1,80x12,0,12{40x12,0,12,2,40x12,40,12,3}]")
                .unwrap();
        assert_eq!(80, layout.width);
        assert_eq!(24, layout.height);

        let vert_children = layout.content.vertical();
        assert_eq!(2, vert_children.len());

        // First child is a simple pane
        assert_eq!(1, vert_children[0].content.pane());

        // Second child is a horizontal split
        let horiz_children = vert_children[1].content.horizontal();
        assert_eq!(2, horiz_children.len());
        assert_eq!(2, horiz_children[0].content.pane());
        assert_eq!(3, horiz_children[1].content.pane());
    }

    #[test]
    fn nested_vertical_in_horizontal() {
        // Horizontal split with left pane and right vertical split
        let layout =
            Layout::parse(b"80x24,0,0{40x24,0,0,1,40x24,40,0[40x12,40,0,2,40x12,40,12,3]}")
                .unwrap();
        assert_eq!(80, layout.width);
        assert_eq!(24, layout.height);

        let horiz_children = layout.content.horizontal();
        assert_eq!(2, horiz_children.len());

        // First child is a simple pane
        assert_eq!(1, horiz_children[0].content.pane());

        // Second child is a vertical split
        let vert_children = horiz_children[1].content.vertical();
        assert_eq!(2, vert_children.len());
        assert_eq!(2, vert_children[0].content.pane());
        assert_eq!(3, vert_children[1].content.pane());
    }

    #[test]
    fn deeply_nested_layout() {
        // Three levels deep
        let layout =
            Layout::parse(b"80x24,0,0{40x24,0,0[40x12,0,0,1,40x12,0,12,2],40x24,40,0,3}")
                .unwrap();

        let horiz = layout.content.horizontal();
        assert_eq!(2, horiz.len());

        let vert = horiz[0].content.vertical();
        assert_eq!(2, vert.len());
        assert_eq!(1, vert[0].content.pane());
        assert_eq!(2, vert[1].content.pane());

        assert_eq!(3, horiz[1].content.pane());
    }

    #[test]
    fn syntax_error_empty_string() {
        assert_eq!(ParseError::SyntaxError, err(b""));
    }

    #[test]
    fn syntax_error_missing_width() {
        assert_eq!(ParseError::SyntaxError, err(b"x24,0,0,1"));
    }

    #[test]
    fn syntax_error_missing_height() {
        assert_eq!(ParseError::SyntaxError, err(b"80x,0,0,1"));
    }

    #[test]
    fn syntax_error_zero_width() {
        assert_eq!(ParseError::SyntaxError, err(b"0x24,0,0,1"));
    }

    #[test]
    fn syntax_error_zero_height() {
        assert_eq!(ParseError::SyntaxError, err(b"80x0,0,0,1"));
    }

    #[test]
    fn syntax_error_zero_size_child_in_split() {
        assert_eq!(ParseError::SyntaxError, err(b"80x24,0,0{40x24,0,0,1,0x24,40,0,2}"));
        assert_eq!(ParseError::SyntaxError, err(b"80x24,0,0[80x12,0,0,1,80x0,0,13,2]"));
    }

    #[test]
    fn parse_with_checksum_zero_size_with_valid_checksum() {
        // Sanity check that the checksum is valid so we're testing the
        // size validation and not the checksum validation.
        let str: &[u8] = b"83x44,0,0[83x22,0,0,0,0x21,0,23,2]";
        assert_eq!(b"52aa", &Checksum::calculate(str).as_string());
        let mut full = b"52aa,".to_vec();
        full.extend_from_slice(str);
        assert_eq!(
            Err(ParseError::SyntaxError),
            Layout::parse_with_checksum(&full)
        );
    }

    #[test]
    fn syntax_error_missing_x() {
        assert_eq!(ParseError::SyntaxError, err(b"80x24,,0,1"));
    }

    #[test]
    fn syntax_error_missing_y() {
        assert_eq!(ParseError::SyntaxError, err(b"80x24,0,,1"));
    }

    #[test]
    fn syntax_error_missing_pane_id() {
        assert_eq!(ParseError::SyntaxError, err(b"80x24,0,0,"));
    }

    #[test]
    fn syntax_error_non_numeric_width() {
        assert_eq!(ParseError::SyntaxError, err(b"abcx24,0,0,1"));
    }

    #[test]
    fn syntax_error_non_numeric_pane_id() {
        assert_eq!(ParseError::SyntaxError, err(b"80x24,0,0,abc"));
    }

    #[test]
    fn syntax_error_unclosed_horizontal_bracket() {
        assert_eq!(ParseError::SyntaxError, err(b"80x24,0,0{40x24,0,0,1"));
    }

    #[test]
    fn syntax_error_unclosed_vertical_bracket() {
        assert_eq!(ParseError::SyntaxError, err(b"80x24,0,0[40x24,0,0,1"));
    }

    #[test]
    fn syntax_error_mismatched_brackets() {
        assert_eq!(ParseError::SyntaxError, err(b"80x24,0,0{40x24,0,0,1]"));
        assert_eq!(ParseError::SyntaxError, err(b"80x24,0,0[40x24,0,0,1}"));
    }

    #[test]
    fn syntax_error_trailing_data() {
        assert_eq!(ParseError::SyntaxError, err(b"80x24,0,0,1extra"));
    }

    #[test]
    fn syntax_error_no_x_separator() {
        assert_eq!(ParseError::SyntaxError, err(b"8024,0,0,1"));
    }

    #[test]
    fn syntax_error_no_content_delimiter() {
        assert_eq!(ParseError::SyntaxError, err(b"80x24,0,0"));
    }

    // parse_with_checksum tests

    #[test]
    fn parse_with_checksum_valid() {
        let layout =
            Layout::parse_with_checksum(b"f8f9,80x24,0,0{40x24,0,0,1,40x24,40,0,2}").unwrap();
        assert_eq!(80, layout.width);
        assert_eq!(24, layout.height);
    }

    #[test]
    fn parse_with_checksum_mismatch() {
        assert_eq!(
            Err(ParseError::ChecksumMismatch),
            Layout::parse_with_checksum(b"0000,80x24,0,0{40x24,0,0,1,40x24,40,0,2}")
        );
    }

    #[test]
    fn parse_with_checksum_too_short() {
        assert_eq!(
            Err(ParseError::SyntaxError),
            Layout::parse_with_checksum(b"bb62")
        );
        assert_eq!(Err(ParseError::SyntaxError), Layout::parse_with_checksum(b""));
    }

    #[test]
    fn parse_with_checksum_missing_comma() {
        assert_eq!(
            Err(ParseError::SyntaxError),
            Layout::parse_with_checksum(b"bb62x159x48,0,0")
        );
    }

    // Checksum tests

    #[test]
    fn checksum_empty_string() {
        let checksum = Checksum::calculate(b"");
        assert_eq!(0u16, checksum.0);
        assert_eq!(b"0000", &checksum.as_string());
    }

    #[test]
    fn checksum_single_character() {
        // 'A' = 65, first iteration: csum = 0 >> 1 | 0 = 0, then 0 + 65 = 65
        let checksum = Checksum::calculate(b"A");
        assert_eq!(65u16, checksum.0);
        assert_eq!(b"0041", &checksum.as_string());
    }

    #[test]
    fn checksum_two_characters() {
        // 'A' (65): csum = 0, rotate = 0, add 65 => 65
        // 'B' (66): csum = 65, rotate => (65 >> 1) | ((65 & 1) << 15) = 32 | 32768 = 32800
        //           add 66 => 32800 + 66 = 32866
        let checksum = Checksum::calculate(b"AB");
        assert_eq!(32866u16, checksum.0);
        assert_eq!(b"8062", &checksum.as_string());
    }

    #[test]
    fn checksum_simple_layout() {
        let checksum = Checksum::calculate(b"80x24,0,0,42");
        assert_eq!(b"d962", &checksum.as_string());
    }

    #[test]
    fn checksum_horizontal_split_layout() {
        let checksum = Checksum::calculate(b"80x24,0,0{40x24,0,0,1,40x24,40,0,2}");
        assert_eq!(b"f8f9", &checksum.as_string());
    }

    #[test]
    fn checksum_as_string_zero_padding() {
        // Value 0x000f should produce "000f"
        assert_eq!(b"000f", &Checksum(0x000f).as_string());
    }

    #[test]
    fn checksum_as_string_all_digits() {
        // Value 0x1234 should produce "1234"
        assert_eq!(b"1234", &Checksum(0x1234).as_string());
    }

    #[test]
    fn checksum_as_string_with_letters() {
        // Value 0xabcd should produce "abcd"
        assert_eq!(b"abcd", &Checksum(0xabcd).as_string());
    }

    #[test]
    fn checksum_as_string_max_value() {
        // Value 0xffff should produce "ffff"
        assert_eq!(b"ffff", &Checksum(0xffff).as_string());
    }

    #[test]
    fn checksum_wraparound() {
        let checksum = Checksum::calculate(b"\xff\xff\xff\xff\xff\xff\xff\xff");
        assert_eq!(b"03fc", &checksum.as_string());
    }

    #[test]
    fn checksum_deterministic() {
        // Same input should always produce same output
        let str = b"159x48,0,0{79x48,0,0,79x48,80,0}";
        let checksum1 = Checksum::calculate(str);
        let checksum2 = Checksum::calculate(str);
        assert_eq!(checksum1, checksum2);
    }

    #[test]
    fn checksum_different_inputs_different_outputs() {
        let checksum1 = Checksum::calculate(b"80x24,0,0,1");
        let checksum2 = Checksum::calculate(b"80x24,0,0,2");
        assert!(checksum1.0 != checksum2.0);
    }

    #[test]
    fn checksum_known_tmux_layout_bb62() {
        // From tmux documentation: "bb62,159x48,0,0{79x48,0,0,79x48,80,0}"
        // The checksum "bb62" corresponds to the layout "159x48,0,0{79x48,0,0,79x48,80,0}"
        let checksum = Checksum::calculate(b"159x48,0,0{79x48,0,0,79x48,80,0}");
        assert_eq!(b"bb62", &checksum.as_string());
    }
}
