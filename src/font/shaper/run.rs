//! Port of `src/font/shaper/run.zig`.
//!
//! STATUS: ported
//!
//! Port note: the Zig `RunIterator` stores `font.Shaper.RunIteratorHook`
//! (the hook type of the active shaper). Here the iterator is generic over
//! a [`RunIteratorHook`] trait so every shaper (including `noop`) can use
//! it. The hooks' `addCodepoint` can only fail with OOM in Zig, so the
//! trait methods are infallible.

use std::hash::Hasher as _;

use crate::font::collection::{GetFaceError, Index};
use crate::font::shape::RunOptions;
use crate::font::shared_grid::SharedGrid;
use crate::font::{Presentation, Style};
use crate::quirks::inline_assert as assert;
use crate::terminal::kitty::graphics_unicode;
use crate::terminal::page;
use crate::terminal::style::{self, Style as TermStyle};

type Hasher = crate::terminal::hash_map::Wyhash;

/// A single text run. A text run is only valid for one Shaper instance and
/// until the next run is created. A text run never goes across multiple
/// rows in a terminal, so it is guaranteed to always be one line.
#[derive(Clone, Copy)]
pub struct TextRun<'a> {
    /// A unique hash for this run. This can be used to cache the shaping
    /// results. We don't provide a means to compare actual values if the
    /// hash is the same, so we should continue to improve this hash to
    /// lower the chance of hash collisions if they become a problem. If
    /// there are hash collisions, it would result in rendering issues but
    /// the core data would be correct.
    ///
    /// The hash is position-independent within the row by using relative
    /// cluster positions. This allows identical runs in different positions
    /// to share the same cache entry, improving cache efficiency.
    pub hash: u64,

    /// The offset in the row where this run started. This is added to the
    /// X position of the final shaped cells to get the absolute position
    /// in the row where they belong.
    pub offset: u16,

    /// The total number of cells produced by this run.
    pub cells: u16,

    /// The font grid that built this run.
    pub grid: &'a SharedGrid,

    /// The font index to use for the glyphs of this run.
    pub font_index: Index,
}

impl std::fmt::Debug for TextRun<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextRun")
            .field("hash", &self.hash)
            .field("offset", &self.offset)
            .field("cells", &self.cells)
            .field("font_index", &self.font_index)
            .finish_non_exhaustive()
    }
}

/// The hooks a shaper provides to [`RunIterator`] (Zig
/// `Shaper.RunIteratorHook`).
pub trait RunIteratorHook {
    /// Allow the hook to prepare for a new run.
    fn prepare(&mut self);
    /// Add a codepoint (with its cluster) to the current run.
    fn add_codepoint(&mut self, cp: u32, cluster: u32);
    /// Finalize the current run.
    fn finalize(&mut self);
}

/// RunIterator is an iterator that yields text runs.
pub struct RunIterator<'a, H: RunIteratorHook> {
    pub hooks: H,
    pub opts: RunOptions<'a>,
    pub i: usize,
}

/// Zig: the anonymous `font_info` struct in `next`.
#[derive(Debug, Clone, Copy)]
struct FontInfo {
    idx: Index,
    fallback: Option<u32>,
}

impl<'a, H: RunIteratorHook> RunIterator<'a, H> {
    pub fn new(hooks: H, opts: RunOptions<'a>) -> Self {
        RunIterator { hooks, opts, i: 0 }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Option<TextRun<'a>>, GetFaceError> {
        let cells = self.opts.cells;

        // Trim the right side of a row that might be empty
        let max: usize = 'max: {
            for i in 0..cells.len() {
                let rev_i = cells.len() - i - 1;
                if !cells.raw[rev_i].is_empty() {
                    break 'max rev_i + 1;
                }
            }

            0
        };

        // Invisible cells don't have any glyphs rendered,
        // so we explicitly skip them in the shaping process.
        while self.i < max && (cells.raw[self.i].has_styling() && cells.style[self.i].flags.invisible) {
            self.i += 1;
        }

        // We're over at the max
        if self.i >= max {
            return Ok(None);
        }

        // Track the font for our current run
        let mut current_font = Index::default();

        // Allow the hook to prepare
        self.hooks.prepare();

        // Initialize our hash for this run.
        let mut hasher = Hasher::init(0);

        // Let's get our style that we'll expect for the run.
        let style: TermStyle = if cells.raw[self.i].has_styling() {
            cells.style[self.i]
        } else {
            TermStyle::default_value()
        };

        // Go through cell by cell and accumulate while we build our run.
        let mut j: usize = self.i;
        'cells: while j < max {
            // Use relative cluster positions (offset from run start) to make
            // the shaping cache position-independent. This ensures that runs
            // with identical content but different starting positions in the
            // row produce the same hash, enabling cache reuse.
            let cluster = j - self.i;
            let cell: &page::Cell = &cells.raw[j];

            // If we have a selection and we're at a boundary point, then
            // we break the run here.
            if let Some(bounds) = self.opts.selection {
                if j > self.i {
                    if bounds[0] > 0 && j == bounds[0] as usize {
                        break 'cells;
                    }
                    if bounds[1] > 0 && j == bounds[1] as usize + 1 {
                        break 'cells;
                    }
                }
            }

            // If we're a spacer, then we ignore it
            match cell.wide() {
                page::Wide::Narrow | page::Wide::Wide => {}
                page::Wide::SpacerHead | page::Wide::SpacerTail => {
                    j += 1;
                    continue;
                }
            }

            // If our cell attributes are changing, then we split the run.
            // This prevents a single glyph for ">=" to be rendered with
            // one color when the two components have different styling.
            if j > self.i {
                'style: {
                    let prev_cell = cells.raw[j - 1];

                    // If the prev cell and this cell are both plain
                    // codepoints then we check if they are commonly "bad"
                    // ligatures and spit the run if they are.
                    if prev_cell.content_tag() == page::ContentTag::Codepoint
                        && cell.content_tag() == page::ContentTag::Codepoint
                    {
                        let prev_cp = prev_cell.codepoint();
                        match prev_cp {
                            // fl, fi
                            0x66 /* 'f' */ => {
                                let cp = cell.codepoint();
                                if cp == 'l' as u32 || cp == 'i' as u32 {
                                    break 'cells;
                                }
                            }

                            // st
                            0x73 /* 's' */ => {
                                let cp = cell.codepoint();
                                if cp == 't' as u32 {
                                    break 'cells;
                                }
                            }

                            _ => {}
                        }
                    }

                    // If the style is exactly the change then fast path out.
                    if prev_cell.style_id() == cell.style_id() {
                        break 'style;
                    }

                    // The style is different. We allow differing background
                    // styles but any other change results in a new run.
                    let c1 = comparable_style(style);
                    let c2 = comparable_style(if cell.has_styling() {
                        cells.style[j]
                    } else {
                        TermStyle::default_value()
                    });
                    if !c1.eql(&c2) {
                        break 'cells;
                    }
                }
            }

            // Text runs break when font styles change so we need to get
            // the proper style.
            let font_style: Style = 'style: {
                if style.flags.bold {
                    if style.flags.italic {
                        break 'style Style::BoldItalic;
                    }
                    break 'style Style::Bold;
                }

                if style.flags.italic {
                    break 'style Style::Italic;
                }
                Style::Regular
            };

            // Determine the presentation format for this glyph.
            let presentation: Option<Presentation> = if cell.has_grapheme() {
                // We only check the FIRST codepoint because I believe the
                // presentation format must be directly adjacent to the codepoint.
                let cps = cells.grapheme(j);
                assert(!cps.is_empty());
                if cps[0] == 0xFE0E {
                    Some(Presentation::Text)
                } else if cps[0] == 0xFE0F {
                    Some(Presentation::Emoji)
                } else {
                    None
                }
            } else {
                // If we're not a grapheme, our individual char could be
                // an emoji so we want to check if we expect emoji presentation.
                // The font grid indexForCodepoint we use below will do this
                // automatically.
                None
            };

            // If our cursor is on this line then we break the run around the
            // cursor. This means that any row with a cursor has at least
            // three breaks: before, exactly the cursor, and after.
            //
            // We do not break a cell that is exactly the grapheme. If there
            // are cells following that contain joiners, we allow those to
            // break. This creates an effect where hovering over an emoji
            // such as a skin-tone emoji is fine, but hovering over the
            // joiners will show the joiners allowing you to modify the
            // emoji.
            if !cell.has_grapheme() {
                if let Some(cursor_x) = self.opts.cursor_x {
                    // Exactly: self.i is the cursor and we iterated once. This
                    // means that we started exactly at the cursor and did at
                    // exactly one iteration. Why exactly one? Because we may
                    // start at our cursor but do many if our cursor is exactly
                    // on an emoji.
                    if self.i == cursor_x && j == self.i + 1 {
                        break 'cells;
                    }

                    // Before: up to and not including the cursor. This means
                    // that we started before the cursor (self.i < cursor_x)
                    // and j is now at the cursor meaning we haven't yet processed
                    // the cursor.
                    if self.i < cursor_x && j == cursor_x {
                        assert(j > 0);
                        break 'cells;
                    }

                    // After: after the cursor. We don't need to do anything
                    // special, we just let the run complete.
                }
            }

            // We need to find a font that supports this character. If
            // there are additional zero-width codepoints (to form a single
            // grapheme, i.e. combining characters), we need to find a font
            // that supports all of them.
            let font_info: FontInfo = 'font_info: {
                // If we find a font that supports this entire grapheme
                // then we use that.
                if let Some(idx) =
                    self.index_for_cell(cell, cells.grapheme(j), font_style, presentation)?
                {
                    break 'font_info FontInfo { idx, fallback: None };
                }

                // Otherwise we need a fallback character. Prefer the
                // official replacement character.
                if let Some(idx) = self.opts.grid.get_index(
                    0xFFFD, // replacement char
                    font_style,
                    presentation,
                )? {
                    break 'font_info FontInfo {
                        idx,
                        fallback: Some(0xFFFD),
                    };
                }

                // Fallback to space
                if let Some(idx) = self.opts.grid.get_index(' ' as u32, font_style, presentation)? {
                    break 'font_info FontInfo {
                        idx,
                        fallback: Some(' ' as u32),
                    };
                }

                // We can't render at all. This is a bug, we should always
                // have a font that can render a space.
                unreachable!();
            };

            //log.warn("char={x} info={}", .{ cell.char, font_info });
            if j == self.i {
                current_font = font_info.idx;
            }

            // If our fonts are not equal, then we're done with our run.
            if font_info.idx != current_font {
                break 'cells;
            }

            // If we're a fallback character, add that and continue; we
            // don't want to add the entire grapheme.
            if let Some(cp) = font_info.fallback {
                self.add_codepoint(&mut hasher, cp, cluster as u32);
                j += 1;
                continue;
            }

            // If we're a Kitty unicode placeholder then we add a blank.
            if cell.codepoint() == graphics_unicode::PLACEHOLDER {
                self.add_codepoint(&mut hasher, ' ' as u32, cluster as u32);
                j += 1;
                continue;
            }

            // Add all the codepoints for our grapheme
            self.add_codepoint(
                &mut hasher,
                if cell.codepoint() == 0 { ' ' as u32 } else { cell.codepoint() },
                cluster as u32,
            );
            if cell.has_grapheme() {
                for &cp in cells.grapheme(j) {
                    // Do not send presentation modifiers
                    if cp == 0xFE0E || cp == 0xFE0F {
                        continue;
                    }
                    self.add_codepoint(&mut hasher, cp, cluster as u32);
                }
            }

            j += 1;
        }

        Ok(Some(self.finish_run(hasher, j, current_font)))
    }

    /// The end of `next` (after the cell loop).
    fn finish_run(&mut self, mut hasher: Hasher, j: usize, current_font: Index) -> TextRun<'a> {
        // Finalize our buffer
        self.hooks.finalize();

        // Add our length to the hash as an additional mechanism to avoid collisions
        hasher.write(&((j - self.i) as u64).to_le_bytes());

        // Add our font index
        hasher.write(&current_font.int().to_le_bytes());

        let run = TextRun {
            hash: hasher.final_(),
            offset: self.i as u16,
            cells: (j - self.i) as u16,
            grid: self.opts.grid,
            font_index: current_font,
        };

        // Move our cursor.
        self.i = j;

        run
    }

    fn add_codepoint(&mut self, hasher: &mut Hasher, cp: u32, cluster: u32) {
        hasher.write(&cp.to_le_bytes());
        hasher.write(&cluster.to_le_bytes());
        self.hooks.add_codepoint(cp, cluster);
    }

    /// Find a font index that supports the grapheme for the given cell,
    /// or null if no such font exists.
    ///
    /// This is used to find a font that supports the entire grapheme.
    /// We look for fonts that support each individual codepoint and then
    /// find the common font amongst all candidates.
    fn index_for_cell(
        &mut self,
        cell: &page::Cell,
        graphemes: &[u32],
        style: Style,
        presentation: Option<Presentation>,
    ) -> Result<Option<Index>, GetFaceError> {
        if cell.is_empty() || cell.codepoint() == 0 || cell.codepoint() == graphics_unicode::PLACEHOLDER {
            return self.opts.grid.get_index(' ' as u32, style, presentation);
        }

        // Get the font index for the primary codepoint.
        let primary_cp: u32 = cell.codepoint();
        let Some(primary) = self.opts.grid.get_index(primary_cp, style, presentation)? else {
            return Ok(None);
        };

        // Easy, and common: we aren't a multi-codepoint grapheme, so
        // we just return whatever index for the cell codepoint.
        if !cell.has_grapheme() {
            return Ok(Some(primary));
        }

        // Try the primary font followed by the font selected for each
        // additional codepoint until one supports the entire grapheme.
        'candidate: for i in 0..graphemes.len() + 1 {
            let idx = if i == 0 {
                primary
            } else {
                let cp = graphemes[i - 1];

                // Ignore Emoji ZWJs
                if cp == 0xFE0E || cp == 0xFE0F || cp == 0x200D {
                    continue 'candidate;
                }

                // We explicitly do not require the additional grapheme components
                // to support the base presentation, since it is common for emoji
                // fonts to support the base emoji with emoji presentation but not
                // certain ZWJ-combined characters like the male and female signs.
                let Some(idx) = self.opts.grid.get_index(cp, style, None)? else {
                    return Ok(None);
                };
                idx
            };

            if !self.opts.grid.has_codepoint(idx, primary_cp, presentation) {
                continue;
            }
            for &cp in graphemes {
                // Ignore Emoji ZWJs
                if cp == 0xFE0E || cp == 0xFE0F || cp == 0x200D {
                    continue;
                }
                if !self.opts.grid.has_codepoint(idx, cp, None) {
                    continue 'candidate;
                }
            }
            return Ok(Some(idx));
        }

        Ok(None)
    }
}

/// Returns a style that when compared must be identical for a run to
/// continue.
fn comparable_style(style: TermStyle) -> TermStyle {
    let mut s = style;

    // We allow background colors to differ because we'll just paint the
    // cell background whatever the style is, and wherever the glyph
    // lands on top of it will be the color of the glyph.
    s.bg_color = style::Color::None;

    s
}
