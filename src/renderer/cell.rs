//! Port of `src/renderer/cell.zig`.
//!
//! STATUS: ported
//!
//! Deviations:
//! * `Key.CellType` (a comptime type function) has no Rust counterpart:
//!   every foreground key stores a [`shaderpkg::CellText`], and
//!   [`Contents::add`] takes the cell directly (`Key::Bg` is unreachable
//!   there, as in Zig).
//! * The allocator parameters are dropped (`Vec` uses the global
//!   allocator).

use crate::renderer::cursor::Style as CursorStyle;
use crate::renderer::graphics_api::shaders as shaderpkg;
use crate::renderer::size::GridSize;
use crate::terminal::page;
use crate::terminal::size::CellCountInt;
use crate::unicode::symbols_table::TABLE as SYMBOLS;

type CellTextRow = Vec<shaderpkg::CellText>;

/// The possible cell content keys that exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    Bg,
    Text,
    Underline,
    Strikethrough,
    Overline,
}

/// The contents of all the cells in the terminal.
///
/// The goal of this data structure is to allow for efficient row-wise
/// clearing of data from the GPU buffers, to allow for row-wise dirty
/// tracking to eliminate the overhead of rebuilding the GPU buffers
/// each frame.
///
/// Must be initialized by resizing before calling any operations.
#[derive(Debug, Clone, Default)]
pub struct Contents {
    pub size: GridSize,

    /// Flat array containing cell background colors for the terminal grid.
    ///
    /// Indexed as `bg_cells[row * size.columns + col]`.
    ///
    /// Prefer accessing with `Contents.bgCell(row, col).*` instead
    /// of directly indexing in order to avoid integer size bugs.
    pub bg_cells: Vec<shaderpkg::CellBg>,

    /// The lists which hold all of the foreground cells. When sized with
    /// Contents.resize the individual ArrayLists are given enough room that
    /// they can hold a single row with #cols glyphs, underlines, and
    /// strikethroughs; however, appendAssumeCapacity MUST NOT be used since
    /// it is possible to exceed this with combining glyphs that add a glyph
    /// but take up no column since they combine with the previous one, as
    /// well as with fonts that perform multi-substitutions for glyphs, which
    /// can result in a similar situation where multiple glyphs reside in the
    /// same column.
    ///
    /// Allocations should nevertheless be exceedingly rare since hitting the
    /// initial capacity of a list would require a row filled with underlined
    /// struck through characters, at least one of which is a multi-glyph
    /// composite.
    ///
    /// Rows are indexed as Contents.fg_rows[y + 1], because the first list in
    /// the collection is reserved for the cursor, which must be the first item
    /// in the buffer.
    ///
    /// Must be initialized by calling resize on the Contents struct before
    /// calling any operations.
    pub fg_rows: Vec<CellTextRow>,
}

impl Contents {
    pub fn deinit(&mut self) {
        *self = Contents::default();
    }

    /// Resize the cell contents for the given grid size. This will
    /// always invalidate the entire cell contents.
    pub fn resize(&mut self, size: GridSize) {
        let row_count: usize = size.rows as usize;

        // The two extra lists hold cursor cells: index 0 is drawn before the
        // row contents, and index row_count + 1 is drawn after them.
        let mut fg_rows: Vec<CellTextRow> = Vec::with_capacity(row_count + 2);

        // Foreground rows hold glyphs plus underlines, overlines, and
        // strikethroughs. Three entries per column cover the common cases
        // without reserving space for every decoration combination. We can't
        // assume this capacity because combining glyphs and font substitutions
        // can produce arbitrarily many glyphs in one column.
        let fg_row_capacity = size.columns as usize * 3;

        // The cursor lists need just one cell. The rest get the full capacity.
        fg_rows.push(Vec::with_capacity(1));
        for _ in 0..row_count {
            fg_rows.push(Vec::with_capacity(fg_row_capacity));
        }
        fg_rows.push(Vec::with_capacity(1));

        let mut bg_cells = std::mem::take(&mut self.bg_cells);
        bg_cells.resize(row_count * size.columns as usize, [0, 0, 0, 0]);

        // Perform the swap, no going back from here.
        self.size = size;
        self.bg_cells = bg_cells;
        self.fg_rows = fg_rows;
        self.reset();
    }

    /// Reset the cell contents to an empty state without resizing.
    pub fn reset(&mut self) {
        self.bg_cells.fill([0, 0, 0, 0]);
        for row in &mut self.fg_rows {
            row.clear();
        }
    }

    /// Set the cursor value. If the value is null then the cursor is hidden.
    pub fn set_cursor(&mut self, v: Option<shaderpkg::CellText>, cursor_style: Option<CursorStyle>) {
        if self.size.rows == 0 {
            return;
        }
        let last = self.size.rows as usize + 1;
        self.fg_rows[0].clear();
        self.fg_rows[last].clear();

        let Some(cell) = v else { return };
        let Some(style) = cursor_style else { return };

        match style {
            // Block cursors should be drawn first
            CursorStyle::Block => self.fg_rows[0].push(cell),
            // Other cursor styles should be drawn last
            CursorStyle::BlockHollow | CursorStyle::Bar | CursorStyle::Underline | CursorStyle::Lock => {
                self.fg_rows[last].push(cell)
            }
        }
    }

    /// Returns the current cursor glyph if present, checking both cursor lists.
    pub fn get_cursor_glyph(&self) -> Option<shaderpkg::CellText> {
        if self.size.rows == 0 {
            return None;
        }
        if let Some(c) = self.fg_rows[0].first() {
            return Some(*c);
        }
        if let Some(c) = self.fg_rows[self.size.rows as usize + 1].first() {
            return Some(*c);
        }
        None
    }

    /// Access a background cell. Prefer this function over direct indexing
    /// of `bg_cells` in order to avoid integer size bugs causing overflows.
    #[inline]
    pub fn bg_cell(&mut self, row: usize, col: usize) -> &mut shaderpkg::CellBg {
        &mut self.bg_cells[row * self.size.columns as usize + col]
    }

    /// Add a cell to the appropriate list. Adding the same cell twice will
    /// result in duplication in the vertex buffer. The caller should clear
    /// the corresponding row with Contents.clear to remove old cells first.
    pub fn add(&mut self, key: Key, cell: shaderpkg::CellText) {
        let y = cell.grid_pos[1];

        debug_assert!(y < self.size.rows);

        match key {
            Key::Bg => unreachable!(),

            // We have a special list containing the cursor cell at the start
            // of our fg row collection, so we need to add 1 to the y to get
            // the correct index.
            Key::Text | Key::Underline | Key::Strikethrough | Key::Overline => {
                self.fg_rows[y as usize + 1].push(cell)
            }
        }
    }

    /// Clear all of the cell contents for a given row.
    pub fn clear(&mut self, y: CellCountInt) {
        debug_assert!(y < self.size.rows);

        let cols = self.size.columns as usize;
        let start = y as usize * cols;
        self.bg_cells[start..start + cols].fill([0, 0, 0, 0]);

        // We have a special list containing the cursor cell at the start
        // of our fg row collection, so we need to add 1 to the y to get
        // the correct index.
        self.fg_rows[y as usize + 1].clear();
    }
}

/// Returns true if a codepoint for a cell is a covering character. A covering
/// character is a character that covers the entire cell. This is used to
/// make window-padding-color=extend work better. See #2099.
pub fn is_covering(cp: u32) -> bool {
    // U+2588 FULL BLOCK
    matches!(cp, 0x2588)
}

/// Returns true of the codepoint is a "symbol-like" character, which
/// for now we define as anything in a private use area, and anything
/// in several unicode blocks:
/// - Arrows
/// - Dingbats
/// - Emoticons
/// - Miscellaneous Symbols
/// - Enclosed Alphanumerics
/// - Enclosed Alphanumeric Supplement
/// - Miscellaneous Symbols and Pictographs
/// - Transport and Map Symbols
///
/// In the future it may be prudent to expand this to encompass more
/// symbol-like characters, and/or exclude some PUA sections.
pub fn is_symbol(cp: u32) -> bool {
    SYMBOLS.get(cp)
}

/// Returns the appropriate `constraint_width` for
/// the provided cell when rendering its glyph(s).
pub fn constraint_width(raw_slice: &[page::Cell], x: usize, cols: usize) -> u8 {
    let cell = raw_slice[x];
    let cp = cell.codepoint();

    let grid_width = cell.grid_width();

    // If the grid width of the cell is 2, the constraint
    // width will always be 2, so we can just return early.
    if grid_width > 1 {
        return grid_width;
    }

    // We allow "symbol-like" glyphs to extend to 2 cells wide if there's
    // space, and if the previous glyph wasn't also a symbol. So if this
    // codepoint isn't a symbol then we can return the grid width.
    if !is_symbol(cp) {
        return grid_width;
    }

    // If we are at the end of the screen it must be constrained to one cell.
    if x == cols - 1 {
        return 1;
    }

    // If we have a previous cell and it was a symbol then we need
    // to also constrain. This is so that multiple PUA glyphs align.
    // This does not apply if the previous symbol is a graphics
    // element such as a block element or Powerline glyph.
    if x > 0 {
        let prev_cp = raw_slice[x - 1].codepoint();
        if is_symbol(prev_cp) && !is_graphics_element(prev_cp) {
            return 1;
        }
    }

    // If the next cell is whitespace, then we
    // allow the glyph to be up to two cells wide.
    let next_cp = raw_slice[x + 1].codepoint();
    if next_cp == 0 || is_space(next_cp) {
        return 2;
    }

    // Otherwise, this has to be 1 cell wide.
    1
}

/// Whether min contrast should be disabled for a given glyph. True
/// for graphics elements such as blocks and Powerline glyphs.
pub fn no_min_contrast(cp: u32) -> bool {
    is_graphics_element(cp)
}

// Some general spaces, others intentionally kept
// to force the font to render as a fixed width.
fn is_space(char: u32) -> bool {
    matches!(
        char,
        0x0020 // SPACE
        | 0x2002 // EN SPACE
    )
}

/// Returns true if the codepoint is used for terminal graphics, such
/// as box drawing characters, block elements, and Powerline glyphs.
fn is_graphics_element(char: u32) -> bool {
    is_box_drawing(char) || is_block_element(char) || is_legacy_computing(char) || is_powerline(char)
}

// Returns true if the codepoint is a box drawing character.
fn is_box_drawing(char: u32) -> bool {
    matches!(char, 0x2500..=0x257F)
}

// Returns true if the codepoint is a block element.
fn is_block_element(char: u32) -> bool {
    matches!(char, 0x2580..=0x259F)
}

// Returns true if the codepoint is in a Symbols for Legacy
// Computing block, including supplements.
fn is_legacy_computing(char: u32) -> bool {
    matches!(char, 0x1FB00..=0x1FBFF | 0x1CC00..=0x1CEBF) // Supplement introduced in Unicode 16.0
}

// Returns true if the codepoint is a part of the Powerline range.
fn is_powerline(char: u32) -> bool {
    matches!(char, 0xE0B0..=0xE0D7)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::graphics_api::shaders::{CellText, CellTextAtlas, CellTextBools};
    use crate::terminal::render::RenderState;
    use crate::terminal::terminal::{Options as TerminalOptions, Terminal};

    fn text_cell(grid_pos: [u16; 2]) -> CellText {
        CellText::new(grid_pos, [0, 0, 0, 1], CellTextAtlas::Grayscale)
    }

    #[test]
    fn contents() {
        let rows = 10;
        let cols = 10;

        let mut c = Contents::default();
        c.resize(GridSize { rows, columns: cols });

        // We should start off empty after resizing.
        for y in 0..rows as usize {
            assert!(c.fg_rows[y + 1].is_empty());
            for x in 0..cols as usize {
                assert_eq!([0, 0, 0, 0], *c.bg_cell(y, x));
            }
        }
        // And the cursor row should have a capacity of 1 and also be empty.
        assert!(c.fg_rows[0].capacity() == 1);
        assert!(c.fg_rows[0].is_empty());

        // Add some contents.
        let bg_cell: shaderpkg::CellBg = [0, 0, 0, 1];
        let fg_cell = text_cell([4, 1]);
        *c.bg_cell(1, 4) = bg_cell;
        c.add(Key::Text, fg_cell);
        assert_eq!(bg_cell, *c.bg_cell(1, 4));
        // The fg row index is offset by 1 because of the cursor list.
        assert_eq!(fg_cell, c.fg_rows[2][0]);

        // And we should be able to clear it.
        c.clear(1);
        for y in 0..rows as usize {
            assert!(c.fg_rows[y + 1].is_empty());
            for x in 0..cols as usize {
                assert_eq!([0, 0, 0, 0], *c.bg_cell(y, x));
            }
        }

        // Add a block cursor.
        let mut cursor_cell = text_cell([2, 3]);
        cursor_cell.bools = CellTextBools::new(false, true);
        c.set_cursor(Some(cursor_cell), Some(CursorStyle::Block));
        assert_eq!(cursor_cell, c.fg_rows[0][0]);
        assert_eq!(cursor_cell, c.get_cursor_glyph().unwrap());

        // And remove it.
        c.set_cursor(None, None);
        assert_eq!(0, c.fg_rows[0].len());
        assert!(c.get_cursor_glyph().is_none());

        // Add a hollow cursor.
        c.set_cursor(Some(cursor_cell), Some(CursorStyle::BlockHollow));
        assert_eq!(cursor_cell, c.fg_rows[rows as usize + 1][0]);
        assert_eq!(cursor_cell, c.get_cursor_glyph().unwrap());
    }

    #[test]
    fn contents_resize_grows_and_shrinks() {
        let mut c = Contents::default();

        c.resize(GridSize { rows: 2, columns: 2 });
        let mut cell = text_cell([1, 1]);
        c.add(Key::Text, cell);
        *c.bg_cell(1, 1) = [0, 0, 0, 1];
        c.set_cursor(Some(cell), Some(CursorStyle::Bar));

        c.resize(GridSize { rows: 3, columns: 4 });
        assert_eq!([0, 0, 0, 0], *c.bg_cell(1, 1));
        assert_eq!(0usize, c.fg_rows[2].len());
        assert_eq!(None, c.get_cursor_glyph());
        cell.grid_pos = [3, 2];
        c.add(Key::Text, cell);

        c.resize(GridSize { rows: 1, columns: 1 });
        cell.grid_pos = [0, 0];
        c.add(Key::Text, cell);
    }

    #[test]
    fn contents_clear_retains_other_content() {
        let rows = 10;
        let cols = 10;

        let mut c = Contents::default();
        c.resize(GridSize { rows, columns: cols });

        // Set some contents
        // bg and fg cells in row 1
        let bg_cell_1: shaderpkg::CellBg = [0, 0, 0, 1];
        let fg_cell_1 = text_cell([4, 1]);
        *c.bg_cell(1, 4) = bg_cell_1;
        c.add(Key::Text, fg_cell_1);
        // bg and fg cells in row 2
        let bg_cell_2: shaderpkg::CellBg = [0, 0, 0, 1];
        let fg_cell_2 = text_cell([4, 2]);
        *c.bg_cell(2, 4) = bg_cell_2;
        c.add(Key::Text, fg_cell_2);

        // Clear row 1, this should leave row 2 untouched
        c.clear(1);

        // Row 2 should still contain its cells.
        assert_eq!(bg_cell_2, *c.bg_cell(2, 4));
        // Fg row index is +1 because of cursor list at start
        assert_eq!(fg_cell_2, c.fg_rows[3][0]);
    }

    #[test]
    fn contents_clear_last_added_content() {
        let rows = 10;
        let cols = 10;

        let mut c = Contents::default();
        c.resize(GridSize { rows, columns: cols });

        // Set some contents
        // bg and fg cells in row 1
        let bg_cell_1: shaderpkg::CellBg = [0, 0, 0, 1];
        let fg_cell_1 = text_cell([4, 1]);
        *c.bg_cell(1, 4) = bg_cell_1;
        c.add(Key::Text, fg_cell_1);
        // bg and fg cells in row 2
        let bg_cell_2: shaderpkg::CellBg = [0, 0, 0, 1];
        let fg_cell_2 = text_cell([4, 2]);
        *c.bg_cell(2, 4) = bg_cell_2;
        c.add(Key::Text, fg_cell_2);

        // Clear row 2, this should leave row 1 untouched
        c.clear(2);

        // Row 1 should still contain its cells.
        assert_eq!(bg_cell_1, *c.bg_cell(1, 4));
        // Fg row index is +1 because of cursor list at start
        assert_eq!(fg_cell_1, c.fg_rows[2][0]);
    }

    #[test]
    fn contents_with_zero_sized_screen() {
        let mut c = Contents::default();

        c.set_cursor(None, None);
        assert!(c.get_cursor_glyph().is_none());
    }

    #[test]
    fn cell_constraint_widths() {
        let mut t = Terminal::init(TerminalOptions::new(4, 1)).unwrap();
        let mut state = RenderState::empty();

        fn check(t: &mut Terminal, state: &mut RenderState, input: &str, cases: &[(usize, u8)]) {
            t.full_reset();
            {
                let mut s = t.vt_stream();
                s.next_slice(input.as_bytes());
                s.deinit();
            }
            state.update(t);
            for &(x, expected) in cases {
                assert_eq!(
                    expected,
                    constraint_width(&state.row_data.get(0).cells.raw, x, state.cols as usize),
                    "input={input:?} x={x}"
                );
            }
        }

        // for each case, the numbers in the comment denote expected
        // constraint widths for the symbol-containing cells

        // symbol->nothing: 2
        check(&mut t, &mut state, "\u{e8ef}", &[(0, 2)]);

        // symbol->character: 1
        check(&mut t, &mut state, "\u{e8ef}z", &[(0, 1)]);

        // symbol->space: 2
        check(&mut t, &mut state, "\u{e8ef} z", &[(0, 2)]);

        // symbol->no-break space: 1
        check(&mut t, &mut state, "\u{e8ef}\u{00a0}z", &[(0, 1)]);

        // symbol->end of row: 1
        check(&mut t, &mut state, "   \u{e8ef}", &[(3, 1)]);

        // character->symbol: 2
        check(&mut t, &mut state, "z\u{e8ef}", &[(1, 2)]);

        // symbol->symbol: 1,1
        check(&mut t, &mut state, "\u{e8ef}\u{e8ef}", &[(0, 1), (1, 1)]);

        // symbol->space->symbol: 2,2
        check(&mut t, &mut state, "\u{e8ef} \u{e8ef}", &[(0, 2), (2, 2)]);

        // symbol->powerline: 1  (dedicated test because powerline is special-cased in cellpkg)
        check(&mut t, &mut state, "\u{e8ef}\u{e0b0}", &[(0, 1)]);

        // powerline->symbol: 2  (dedicated test because powerline is special-cased in cellpkg)
        check(&mut t, &mut state, "\u{e0b2}\u{e8ef}", &[(1, 2)]);

        // powerline->nothing: 2  (dedicated test because powerline is special-cased in cellpkg)
        check(&mut t, &mut state, "\u{e0b2}", &[(0, 2)]);

        // powerline->space: 2  (dedicated test because powerline is special-cased in cellpkg)
        check(&mut t, &mut state, "\u{e0b2} z", &[(0, 2)]);
    }
}
