//! Port of `src/renderer/row.zig`.
//!
//! STATUS: ported

use crate::terminal::color;
use crate::terminal::page::{self, ContentTag, SemanticPrompt};
use crate::terminal::style::Style;

// TODO: Test neverExtendBg function

/// Returns true if the row of this pin should never have its background
/// color extended for filling padding space in the renderer. This is
/// a set of heuristics that help making our padding look better.
pub fn never_extend_bg(
    row: page::Row,
    cells: &[page::Cell],
    styles: &[Style],
    palette: &color::Palette,
    default_background: color::Rgb,
) -> bool {
    // Any semantic prompts should not have their background extended
    // because prompts often contain special formatting (such as
    // powerline) that looks bad when extended.
    match row.semantic_prompt() {
        SemanticPrompt::Prompt | SemanticPrompt::PromptContinuation => return true,
        SemanticPrompt::None => {}
    }

    for (x, cell) in cells.iter().enumerate() {
        // If any cell has a default background color then we don't
        // extend because the default background color probably looks
        // good enough as an extension.
        match cell.content_tag() {
            // If it is a background color cell, we check the color.
            ContentTag::BgColorPalette | ContentTag::BgColorRgb => {
                let s: Style = if cell.has_styling() { styles[x] } else { Style::default() };
                let Some(bg) = s.bg(cell, palette) else {
                    return true;
                };
                if bg.eql(default_background) {
                    return true;
                }
            }

            // If its a codepoint cell we can check the style.
            ContentTag::Codepoint | ContentTag::CodepointGrapheme => {
                // For codepoint containing, we also never extend bg
                // if any cell has a powerline glyph because these are
                // perfect-fit.
                match cell.codepoint() {
                    // Powerline
                    0xE0B0..=0xE0C8 | 0xE0CA | 0xE0CC..=0xE0D2 | 0xE0D4 => return true,
                    _ => {}
                }

                // Never extend a cell that has a default background.
                // A default background is applied if there is no background
                // on the style or the explicitly set background
                // matches our default background.
                let s: Style = if cell.has_styling() { styles[x] } else { Style::default() };
                let Some(bg) = s.bg(cell, palette) else {
                    return true;
                };
                if bg.eql(default_background) {
                    return true;
                }
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::render::RenderState;
    use crate::terminal::terminal::{Options, Terminal};

    /// Write `input` to a fresh 10x3 terminal and return row 0 of its
    /// render state as `(row, cells, styles)`.
    fn render_row(input: &[u8]) -> (page::Row, Vec<page::Cell>, Vec<Style>) {
        let mut t = Terminal::init(Options::new(10, 3)).unwrap();
        {
            let mut s = t.vt_stream();
            s.next_slice(input);
        }
        let mut state = RenderState::empty();
        state.update(&mut t);
        let cells = &state.row_data.cells[0];
        (state.row_data.raw[0], cells.raw.clone(), cells.style.clone())
    }

    // Not a Zig test (the Zig file has a TODO for one).
    #[test]
    fn never_extend_bg_heuristics() {
        let palette: color::Palette = color::DEFAULT;
        let default_bg = color::Rgb { r: 0, g: 0, b: 0 };
        let check = |input: &[u8]| {
            let (row, cells, styles) = render_row(input);
            never_extend_bg(row, &cells, &styles, &palette, default_bg)
        };

        // Prompt rows never extend.
        {
            let (mut row, cells, styles) = render_row(b"\x1b[48;2;255;0;0m> \x1b[K");
            assert!(!never_extend_bg(row, &cells, &styles, &palette, default_bg));
            row.set_semantic_prompt(SemanticPrompt::Prompt);
            assert!(never_extend_bg(row, &cells, &styles, &palette, default_bg));
            row.set_semantic_prompt(SemanticPrompt::PromptContinuation);
            assert!(never_extend_bg(row, &cells, &styles, &palette, default_bg));
        }

        // A row filled with a non-default background extends.
        assert!(!check(b"\x1b[48;2;255;0;0mab\x1b[K"));

        // An unstyled cell has the default background.
        assert!(check(b"\x1b[48;2;255;0;0mab\x1b[0mc"));

        // Powerline glyphs never extend.
        assert!(check("\x1b[48;2;255;0;0m\u{E0B0}\x1b[K".as_bytes()));

        // Background color cells are checked against the default.
        assert!(check(b"\x1b[48;2;0;0;0m\x1b[K"));
        assert!(!check(b"\x1b[48;5;7m\x1b[K"));
    }
}
