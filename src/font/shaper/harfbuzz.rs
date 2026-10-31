//! Port of `src/font/shaper/harfbuzz.zig`.
//!
//! STATUS: ported
//!
//! The Zig tests build `terminal.Terminal` and `terminal.RenderState`
//! instances and feed them through `Terminal::vt_stream`.

use crate::font::collection::{GetFaceError, Index};
use crate::font::shape::{self, Cell, FeatureList, RunOptions, TextRun, DEFAULT_FEATURES};
use crate::font::shaper::run::{self, RunIterator};
use crate::pkg::harfbuzz as hb;
use crate::quirks::inline_assert as assert;

const LOG: &str = "font_shaper";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Codepoint {
    cluster: u32,
    codepoint: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct RunOffset {
    cluster: u32,
    x: i32,
    y: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct CellOffset {
    cluster: u32,
    x: i32,
}

/// Errors from [`Shaper::shape`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeError {
    GetFace(GetFaceError),
    HarfbuzzFailed,
}

impl std::fmt::Display for ShapeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShapeError::GetFace(e) => write!(f, "{e}"),
            ShapeError::HarfbuzzFailed => f.write_str("HarfbuzzFailed"),
        }
    }
}

impl std::error::Error for ShapeError {}

impl From<GetFaceError> for ShapeError {
    fn from(e: GetFaceError) -> ShapeError {
        ShapeError::GetFace(e)
    }
}

/// Shaper that uses Harfbuzz.
pub struct Shaper {
    /// The buffer used for text shaping. We reuse it across multiple shaping
    /// calls to prevent allocations.
    pub hb_buf: hb::Buffer,

    /// The shared memory used for shaping results.
    cell_buf: Vec<Cell>,

    /// The features to use for shaping.
    hb_feats: Vec<hb::Feature>,

    /// The codepoints added to the buffer before shaping. We need to keep
    /// these separately because after shaping, HarfBuzz replaces codepoints
    /// with glyph indices in the buffer.
    codepoints: Vec<Codepoint>,
}

// SAFETY: the HarfBuzz buffer is owned exclusively by this shaper.
unsafe impl Send for Shaper {}

impl Drop for Shaper {
    fn drop(&mut self) {
        // SAFETY: we own the buffer and it isn't used afterwards.
        unsafe { self.hb_buf.destroy() };
    }
}

impl Shaper {
    /// The cell_buf argument is the buffer to use for storing shaped results.
    /// This should be at least the number of columns in the terminal.
    pub fn init(opts: shape::Options<'_>) -> Result<Shaper, hb::Error> {
        // Parse all the features we want to use.
        let hb_feats: Vec<hb::Feature> = {
            let mut feature_list = FeatureList {
                features: DEFAULT_FEATURES.to_vec(),
            };
            for feature_str in opts.features {
                feature_list.append_from_string(feature_str.as_bytes());
            }

            feature_list
                .features
                .iter()
                .map(|feature| hb::Feature {
                    tag: u32::from_be_bytes(feature.tag),
                    value: feature.value,
                    start: hb::c::HB_FEATURE_GLOBAL_START,
                    // HB_FEATURE_GLOBAL_END is `(unsigned int) -1`.
                    end: u32::MAX,
                })
                .collect()
        };

        Ok(Shaper {
            hb_buf: hb::Buffer::create()?,
            cell_buf: Vec::new(),
            hb_feats,
            codepoints: Vec::new(),
        })
    }

    pub fn end_frame(&self) {}

    /// Returns an iterator that returns one text run at a time for the
    /// given terminal row. Note that text runs are only valid one at a time
    /// for a Shaper struct since they share state.
    ///
    /// The selection must be a row-only selection (height = 1). See
    /// Selection.containedRow. The run iterator will ONLY look at X values
    /// and assume the y value matches.
    ///
    /// The shaper is reachable while iterating as `it.hooks.shaper`.
    pub fn run_iterator<'a>(&'a mut self, opts: RunOptions<'a>) -> RunIterator<'a, RunIteratorHook<'a>> {
        RunIterator::new(RunIteratorHook { shaper: self }, opts)
    }

    /// Shape the given text run. The text run must be the immediately previous
    /// text run that was iterated since the text run does share state with the
    /// Shaper struct.
    ///
    /// The return value is only valid until the next shape call is called.
    ///
    /// If there is not enough space in the cell buffer, an error is returned.
    pub fn shape(&mut self, run: TextRun<'_>) -> Result<&[Cell], ShapeError> {
        // We only do shaping if the font is not a special-case. For special-case
        // fonts, the codepoint == glyph_index so we don't need to run any shaping.
        if run.font_index.special().is_none() {
            // We have to lock the grid to get the face and unfortunately
            // freetype faces (typically used with harfbuzz) are not thread
            // safe so this has to be an exclusive lock.
            let mut state = run.grid.lock.write().unwrap_or_else(|e| e.into_inner());

            let face = state.resolver.collection.get_face(run.font_index)?;
            let i = if !face.quirks_disable_default_font_features {
                0
            } else {
                // If we are disabling default font features we just offset
                // our features by the hardcoded items because always
                // add those at the beginning.
                DEFAULT_FEATURES.len()
            };

            // The CoreText face only has a HarfBuzz font when built for
            // the HarfBuzz shaper (Zig: `if (harfbuzz_shaper) ... else void`).
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            let hb_font = face.hb_font.expect("CoreText face has no HarfBuzz font");
            #[cfg(not(any(target_os = "macos", target_os = "ios")))]
            let hb_font = face.hb_font;
            hb::shape(hb_font, self.hb_buf, Some(&self.hb_feats[i..]));
        }

        // If our buffer is empty, we short-circuit the rest of the work
        // return nothing.
        if self.hb_buf.get_length() == 0 {
            return Ok(&self.cell_buf[0..0]);
        }
        let info = self.hb_buf.get_glyph_infos();
        let Some(pos) = self.hb_buf.get_glyph_positions() else {
            return Err(ShapeError::HarfbuzzFailed);
        };

        // This is perhaps not true somewhere, but we currently assume it is true.
        // If it isn't true, I'd like to catch it and learn more.
        assert(info.len() == pos.len());

        // This keeps track of the current x and y offsets (sum of advances)
        // and the furthest cluster we've seen so far (max).
        let mut run_offset = RunOffset::default();

        // This keeps track of the cell starting x and cluster.
        let mut cell_offset = CellOffset::default();

        // Convert all our info/pos to cells and set it.
        self.cell_buf.clear();
        for (info_v, pos_v) in info.iter().zip(pos.iter()) {
            // info_v.cluster is the index into our codepoints array. We use it
            // to get the original cluster.
            let index = info_v.cluster;
            // Our cluster is also our cell X position. If the cluster changes
            // then we need to reset our current cell offsets.
            let cluster = self.codepoints[index as usize].cluster;
            if cell_offset.cluster != cluster {
                let is_after_glyph_from_current_or_next_clusters = cluster <= run_offset.cluster;

                let is_first_codepoint_in_cluster = 'blk: {
                    let mut i = index;
                    if i > 0 {
                        i -= 1;
                        let codepoint = self.codepoints[i as usize];
                        break 'blk codepoint.cluster != cluster;
                    }
                    true
                };

                // We need to reset the `cell_offset` at the start of a new
                // cluster, but we do that conditionally if the codepoint
                // `is_first_codepoint_in_cluster` and the cluster is not
                // `is_after_glyph_from_current_or_next_clusters`, which is
                // a heuristic to detect ligatures and avoid positioning
                // glyphs that mark ligatures incorrectly. The idea is that
                // if the first codepoint in a cluster doesn't appear in
                // the stream, it's very likely that it combined with
                // codepoints from a previous cluster into a ligature.
                // Then, the subsequent codepoints are very likely marking
                // glyphs that are placed relative to that ligature, so if
                // we were to reset the `cell_offset` to align it with the
                // grid, the positions would be off. The
                // `!is_after_glyph_from_current_or_next_clusters` check is
                // needed in case these marking glyphs come from a later
                // cluster but are rendered first (see the Chakma and
                // Bengali tests). In that case when we get to the
                // codepoint that `is_first_codepoint_in_cluster`, but in a
                // cluster that
                // `is_after_glyph_from_current_or_next_clusters`, we don't
                // want to reset to the grid and cause the positions to be
                // off. (Note that we could go back and align the cells to
                // the grid starting from the one from the cluster that
                // rendered out of order, but that is more complicated so
                // we don't do that for now. Also, it's TBD if there are
                // exceptions to this heuristic for detecting ligatures,
                // but using the logging below seems to show it works
                // well.)
                if is_first_codepoint_in_cluster && !is_after_glyph_from_current_or_next_clusters {
                    cell_offset = CellOffset {
                        cluster,
                        x: run_offset.x,
                    };
                }
            }

            // Under both FreeType and CoreText the harfbuzz scale is
            // in 26.6 fixed point units, so we round to the nearest
            // whole value here.
            let x_offset = run_offset.x - cell_offset.x + ((pos_v.x_offset + 0b100_000) >> 6);
            let y_offset = run_offset.y + ((pos_v.y_offset + 0b100_000) >> 6);

            // For debugging positions, turn this on:
            //self.debug_positions(run_offset, cell_offset, *pos_v, index);

            self.cell_buf.push(Cell {
                x: cell_offset.cluster as u16,
                x_offset: x_offset as i16,
                y_offset: y_offset as i16,
                glyph_index: info_v.codepoint,
            });

            // Add our advances to keep track of our run offsets.
            // Advances apply to the NEXT cell.
            // Under both FreeType and CoreText the harfbuzz scale is
            // in 26.6 fixed point units, so we round to the nearest
            // whole value here.
            run_offset.x += (pos_v.x_advance + 0b100_000) >> 6;
            run_offset.y += (pos_v.y_advance + 0b100_000) >> 6;
            run_offset.cluster = run_offset.cluster.max(cluster);

            // const i = self.cell_buf.items.len - 1;
            // log.warn("i={} info={} pos={} cell={}", .{ i, info_v, pos_v, self.cell_buf.items[i] });
        }
        //log.warn("----------------", .{});

        Ok(&self.cell_buf)
    }

    #[allow(dead_code)]
    fn debug_positions(
        &self,
        run_offset: RunOffset,
        cell_offset: CellOffset,
        pos_v: hb::GlyphPosition,
        index: u32,
    ) {
        use std::fmt::Write as _;

        let x_offset = run_offset.x - cell_offset.x + ((pos_v.x_offset + 0b100_000) >> 6);
        let y_offset = run_offset.y + ((pos_v.y_offset + 0b100_000) >> 6);
        let advance_x_offset = run_offset.x - cell_offset.x;
        let advance_y_offset = run_offset.y;
        let x_offset_diff = x_offset - advance_x_offset;
        let y_offset_diff = y_offset - advance_y_offset;
        let positions_differ = x_offset_diff.abs() > 0 || y_offset_diff.abs() > 0;
        let y_offset_differs = run_offset.y != 0;
        let cluster = self.codepoints[index as usize].cluster;
        let cluster_differs = cluster != cell_offset.cluster;

        // To debug every loop, flip this to true:
        let extra_debugging = false;

        let is_previous_codepoint_prepend = if cluster_differs || extra_debugging {
            'blk: {
                let mut i = index;
                if i > 0 {
                    i -= 1;
                    let codepoint = self.codepoints[i as usize];
                    break 'blk crate::unicode::TABLE.get(codepoint.codepoint).grapheme_break
                        == crate::unicode::uucode::types::GraphemeBreakNoControl::Prepend;
                }
                false
            }
        } else {
            false
        };

        let formatted_cps: Option<String> =
            if positions_differ || y_offset_differs || cluster_differs || extra_debugging {
                let mut writer = String::new();
                let codepoints = &self.codepoints;
                let mut last_cluster: Option<u32> = None;
                for (i, cp) in codepoints.iter().enumerate() {
                    if cp.cluster as i32 >= cell_offset.cluster as i32 - 1 && cp.cluster <= cluster + 1 {
                        if let Some(last) = last_cluster {
                            if cp.cluster != last {
                                writer.push(' ');
                            }
                        }
                        if i == index as usize {
                            writer.push('▸');
                        }
                        // Using Python syntax for easier debugging
                        if cp.codepoint > 0xFFFF {
                            let _ = write!(writer, "\\U{:0>8x}", cp.codepoint);
                        } else {
                            let _ = write!(writer, "\\u{:0>4x}", cp.codepoint);
                        }
                        last_cluster = Some(cp.cluster);
                    }
                }
                writer.push_str(" → ");
                for cp in codepoints {
                    if cp.cluster as i32 >= cell_offset.cluster as i32 - 1 && cp.cluster <= cluster + 1 {
                        writer.push(char::from_u32(cp.codepoint).unwrap_or('\u{FFFD}'));
                    }
                }
                Some(writer)
            } else {
                None
            };

        if extra_debugging {
            log::warn!(
                target: LOG,
                "extra debugging of positions index={} cell_offset.cluster={} cluster={} run_offset.cluster={} diff={} pos=({},{}) run_offset=({},{}) cell_offset.x={} is_prev_prepend={} cps = {}",
                index,
                cell_offset.cluster,
                cluster,
                run_offset.cluster,
                cluster as isize - cell_offset.cluster as isize,
                x_offset,
                y_offset,
                run_offset.x,
                run_offset.y,
                cell_offset.x,
                is_previous_codepoint_prepend,
                formatted_cps.as_deref().unwrap(),
            );
        }

        if positions_differ {
            log::warn!(
                target: LOG,
                "position differs from advance: cluster={} pos=({},{}) adv=({},{}) diff=({},{}) cps = {}",
                cluster,
                x_offset,
                y_offset,
                advance_x_offset,
                advance_y_offset,
                x_offset_diff,
                y_offset_diff,
                formatted_cps.as_deref().unwrap(),
            );
        }

        if y_offset_differs {
            log::warn!(
                target: LOG,
                "run_offset.y differs from zero: cluster={} pos=({},{}) run_offset=({},{}) cell_offset.x={} cps = {}",
                cluster,
                x_offset,
                y_offset,
                run_offset.x,
                run_offset.y,
                cell_offset.x,
                formatted_cps.as_deref().unwrap(),
            );
        }

        if cluster_differs {
            log::warn!(
                target: LOG,
                "cell_offset.cluster differs from cluster (potential ligature detected) cell_offset.cluster={} cluster={} run_offset.cluster={} diff={} pos=({},{}) run_offset=({},{}) cell_offset.x={} is_prev_prepend={} cps = {}",
                cell_offset.cluster,
                cluster,
                run_offset.cluster,
                cluster as isize - cell_offset.cluster as isize,
                x_offset,
                y_offset,
                run_offset.x,
                run_offset.y,
                cell_offset.x,
                is_previous_codepoint_prepend,
                formatted_cps.as_deref().unwrap(),
            );
        }
    }
}

/// The hooks for RunIterator.
pub struct RunIteratorHook<'a> {
    pub shaper: &'a mut Shaper,
}

impl run::RunIteratorHook for RunIteratorHook<'_> {
    fn prepare(&mut self) {
        // Reset the buffer for our current run
        self.shaper.hb_buf.reset();
        self.shaper.hb_buf.set_content_type(hb::buffer::ContentType::Unicode);

        // We set the cluster level to `characters` to give us the most
        // granularity, matching the CoreText shaper, and allowing us
        // to use our same ligature detection heuristics.
        self.shaper.hb_buf.set_cluster_level(hb::buffer::ClusterLevel::Characters);

        self.shaper.codepoints.clear();

        // We don't support RTL text because RTL in terminals is messy.
        // Its something we want to improve. For now, we force LTR because
        // our renderers assume a strictly increasing X value.
        self.shaper.hb_buf.set_direction(hb::Direction::Ltr);
    }

    fn add_codepoint(&mut self, cp: u32, cluster: u32) {
        // log.warn("cluster={} cp={x}", .{ cluster, cp });
        // We pass the index into codepoints as the cluster value to HarfBuzz.
        // After shaping, we use info.cluster to get back the index, which
        // lets us look up the original cluster value from codepoints.
        let index = self.shaper.codepoints.len() as u32;
        self.shaper.hb_buf.add(cp, index);
        self.shaper.codepoints.push(Codepoint { cluster, codepoint: cp });
    }

    fn finalize(&mut self) {
        self.shaper.hb_buf.guess_segment_properties();
    }
}

// Silence the unused import warning for `Index` (used in tests/docs).
#[allow(dead_code)]
type _FontIndex = Index;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::font::codepoint_resolver::CodepointResolver;
    use crate::font::collection::{AddOptions, Collection, LoadOptions, SizeAdjustment};
    use crate::font::deferred_face::DeferredFace;
    use crate::font::embedded;
    use crate::font::face::{DesiredSize, Face};
    use crate::font::library::Library;
    use crate::terminal::render::{Cell as RenderCell, Cells};
    use crate::font::shared_grid::SharedGrid;
    use crate::font::{Style, OPTIONS};
    use crate::terminal::page;

    /// Deinitializes the library when dropped (after the grid, which is
    /// declared before it in [`TestShaper`]).
    struct LibGuard(Library);

    impl Drop for LibGuard {
        fn drop(&mut self) {
            self.0.deinit();
        }
    }

    struct TestShaper {
        shaper: Shaper,
        grid: Box<SharedGrid>,
        #[allow(dead_code)]
        lib: LibGuard,
    }

    #[derive(Debug, Clone, Copy)]
    #[allow(dead_code)]
    enum TestFont {
        Inconsolata,
        MonaspaceNeon,
        Arabic,
    }

    fn add_opts() -> AddOptions {
        AddOptions {
            style: Style::Regular,
            fallback: false,
            size_adjustment: SizeAdjustment::None,
        }
    }

    /// Helper to return a fully initialized shaper.
    fn test_shaper() -> TestShaper {
        test_shaper_with_font(TestFont::Inconsolata)
    }

    fn test_shaper_with_font(font_req: TestFont) -> TestShaper {
        let test_emoji = embedded::EMOJI;
        let test_emoji_text = embedded::EMOJI_TEXT;
        let test_font = match font_req {
            TestFont::Inconsolata => embedded::INCONSOLATA,
            TestFont::MonaspaceNeon => embedded::MONASPACE_NEON,
            TestFont::Arabic => embedded::ARABIC,
        };

        let lib = Library::init().unwrap();

        let mut c = Collection::init();
        c.load_options = Some(LoadOptions::new(lib.clone()));

        // Setup group
        c.add(
            Face::init(lib.clone(), test_font, DesiredSize::new(12.0).into()).unwrap(),
            add_opts(),
        )
        .unwrap();

        if !OPTIONS.backend.has_coretext() {
            // Coretext doesn't support Noto's format
            c.add(
                Face::init(lib.clone(), test_emoji, DesiredSize::new(12.0).into()).unwrap(),
                add_opts(),
            )
            .unwrap();
        } else {
            // On CoreText we want to load Apple Emoji, we should have it.
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            {
                let disco = crate::font::Discover::init(&lib);
                let mut disco_it = disco
                    .discover(&crate::font::Descriptor {
                        family: Some("Apple Color Emoji".to_string()),
                        size: 12.0,
                        monospace: false,
                        ..Default::default()
                    })
                    .unwrap();
                let face = disco_it.next().unwrap().unwrap();
                c.add_deferred(face, add_opts()).unwrap();
            }
        }
        c.add(
            Face::init(lib.clone(), test_emoji_text, DesiredSize::new(12.0).into()).unwrap(),
            add_opts(),
        )
        .unwrap();

        let grid = Box::new(SharedGrid::init(CodepointResolver::new(c)).unwrap());

        let shaper = Shaper::init(shape::Options {
            // Some of our tests rely on dlig being enabled by default
            features: &["dlig"],
        })
        .unwrap();

        TestShaper {
            shaper,
            grid,
            lib: LibGuard(lib),
        }
    }

    /// Returns `None` where the Zig version returns `error.SkipZigTest`
    /// or `error.FontNotFound`.
    #[allow(dead_code)]
    fn test_shaper_with_discovered_font(font_req: &str) -> Option<TestShaper> {
        #[cfg(target_arch = "wasm32")]
        {
            let _ = font_req;
            None
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            let lib = Library::init().unwrap();
            let lib = LibGuard(lib);

            let mut c = Collection::init();
            c.load_options = Some(LoadOptions::new(lib.0.clone()));

            // Discover and add our font to the collection.
            {
                let disco = crate::font::Discover::init(&lib.0);
                let mut disco_it = disco
                    .discover(&crate::font::Descriptor {
                        family: Some(font_req.to_string()),
                        size: 12.0,
                        monospace: false,
                        ..Default::default()
                    })
                    .ok()?;
                let mut face: DeferredFace = disco_it.next().ok()??;

                // Check which font was discovered - skip if it doesn't match the request
                let mut name_buf = [0u8; 256];
                let face_name = face.name(&mut name_buf).unwrap_or(b"(unknown)").to_vec();
                if !String::from_utf8_lossy(&face_name).contains(font_req) {
                    return None;
                }

                let loaded = face.load(lib.0.clone(), DesiredSize::new(12.0).into()).unwrap();
                c.add(loaded, add_opts()).unwrap();
            }

            let grid = Box::new(SharedGrid::init(CodepointResolver::new(c)).unwrap());

            let shaper = Shaper::init(shape::Options::default()).unwrap();

            Some(TestShaper { shaper, grid, lib })
        }
    }

    /// Build render cells for a row of plain narrow codepoints (Rust-only
    /// helper: the Zig tests get cells from a terminal render state).
    fn plain_cells(s: &str, cols: usize) -> Cells {
        let mut cells = Cells::default();
        for c in s.chars() {
            cells.append(RenderCell {
                raw: page::Cell::init(c as u32),
                ..Default::default()
            });
        }
        while cells.len() < cols {
            cells.append(RenderCell::default());
        }
        cells
    }

    /// Rust-only smoke test of the run iterator and shaper on hand-built
    /// cells (the ported Zig tests below need `terminal::Terminal`).
    #[test]
    fn shape_plain_cells() {
        let mut testdata = test_shaper();
        let grid: &SharedGrid = &testdata.grid;

        // One run for plain ASCII, including spaces.
        {
            let cells = plain_cells("ABCD   EFG", 10);
            let mut it = testdata.shaper.run_iterator(RunOptions {
                cells: cells.slice(),
                ..RunOptions::new(grid)
            });
            let mut count = 0;
            while let Some(run) = it.next().unwrap() {
                count += 1;
                assert_eq!(10, run.cells);
                let shaped = it.hooks.shaper.shape(run).unwrap();
                assert_eq!(10, shaped.len());
            }
            assert_eq!(1, count);
        }

        // Inconsolata ligature ">=" (2 cells, 1 glyph) and "===".
        for (s, n) in [(">=", 2u16), ("===", 3)] {
            let cells = plain_cells(s, 5);
            let mut it = testdata.shaper.run_iterator(RunOptions {
                cells: cells.slice(),
                ..RunOptions::new(grid)
            });
            let mut count = 0;
            while let Some(run) = it.next().unwrap() {
                count += 1;
                assert_eq!(n, run.cells);
                let shaped = it.hooks.shaper.shape(run).unwrap();
                assert_eq!(1, shaped.len());
            }
            assert_eq!(1, count);
        }

        // Box glyphs use the sprite font and are not shaped.
        {
            let cells = plain_cells("\u{2500}\u{2501}", 10);
            let mut it = testdata.shaper.run_iterator(RunOptions {
                cells: cells.slice(),
                ..RunOptions::new(grid)
            });
            let mut count = 0;
            while let Some(run) = it.next().unwrap() {
                count += 1;
                assert_eq!(2, it.hooks.shaper.hb_buf.get_length());
                let shaped = it.hooks.shaper.shape(run).unwrap();
                assert_eq!(2, shaped.len());
                assert_eq!(0x2500, shaped[0].glyph_index);
                assert_eq!(0, shaped[0].x);
                assert_eq!(0x2501, shaped[1].glyph_index);
                assert_eq!(1, shaped[1].x);
            }
            assert_eq!(1, count);
        }

        // Cursor and selection boundaries split runs.
        {
            let cells = plain_cells("a1b2c3d4e5", 10);
            let count_runs = |shaper: &mut Shaper, opts: RunOptions<'_>| {
                let mut it = shaper.run_iterator(opts);
                let mut count = 0;
                while let Some(run) = it.next().unwrap() {
                    count += 1;
                    it.hooks.shaper.shape(run).unwrap();
                }
                count
            };
            let base = RunOptions {
                cells: cells.slice(),
                ..RunOptions::new(grid)
            };
            assert_eq!(1, count_runs(&mut testdata.shaper, base));
            assert_eq!(2, count_runs(&mut testdata.shaper, RunOptions { cursor_x: Some(0), ..base }));
            assert_eq!(3, count_runs(&mut testdata.shaper, RunOptions { cursor_x: Some(1), ..base }));
            assert_eq!(2, count_runs(&mut testdata.shaper, RunOptions { cursor_x: Some(9), ..base }));
            assert_eq!(1, count_runs(&mut testdata.shaper, RunOptions { selection: Some([0, 9]), ..base }));
            assert_eq!(2, count_runs(&mut testdata.shaper, RunOptions { selection: Some([2, 9]), ..base }));
            assert_eq!(2, count_runs(&mut testdata.shaper, RunOptions { selection: Some([0, 3]), ..base }));
            assert_eq!(3, count_runs(&mut testdata.shaper, RunOptions { selection: Some([1, 3]), ..base }));
            assert_eq!(3, count_runs(&mut testdata.shaper, RunOptions { selection: Some([1, 1]), ..base }));
        }
    }

    // -----------------------------------------------------------------
    // Ported Zig tests. They use terminal::Terminal, its vtStream and
    // terminal::RenderState.
    // -----------------------------------------------------------------

    /// Zig: `state.row_data.get(row).cells.slice()` converted to the
    /// shaper's cell type.
    fn row_cells(state: &crate::terminal::RenderState, row: usize) -> Cells {
        state.row_data.cells[row].clone()
    }

    /// Zig: `Terminal.init(io, alloc, .{ .cols, .rows })`.
    fn new_terminal(cols: u16, rows: u16) -> crate::terminal::Terminal {
        crate::terminal::Terminal::init(crate::terminal::terminal::Options {
            cols,
            rows,
            ..Default::default()
        })
        .unwrap()
    }

    /// Zig: build a terminal, feed `input` through its vt stream, update a
    /// render state and return the cells of `row`.
    fn terminal_row(cols: u16, rows: u16, inputs: &[&[u8]], row: usize) -> Cells {
        let mut t = new_terminal(cols, rows);
        {
            let mut s = t.vt_stream();
            for input in inputs {
                s.next_slice(input);
            }
        }
        let mut state = crate::terminal::RenderState::empty();
        state.update(&mut t);
        row_cells(&state, row)
    }

    /// Count the runs of a row (shaping each one), the most common
    /// pattern in the Zig tests.
    fn count_runs(testdata: &mut TestShaper, opts_fn: impl FnOnce(RunOptions<'_>) -> RunOptions<'_>, cells: &Cells) -> usize {
        let grid: &SharedGrid = &testdata.grid;
        let mut it = testdata.shaper.run_iterator(opts_fn(RunOptions {
            cells: cells.slice(),
            ..RunOptions::new(grid)
        }));
        let mut count = 0;
        while let Some(run) = it.next().unwrap() {
            count += 1;
            it.hooks.shaper.shape(run).unwrap();
        }
        count
    }

    #[test]
    fn run_iterator() {
        let mut testdata = test_shaper();

        {
            // Make a screen with some data
            let cells = terminal_row(5, 3, &[b"ABCD"], 0);

            // Get our run iterator
            let grid: &SharedGrid = &testdata.grid;
            let mut it = testdata.shaper.run_iterator(RunOptions { cells: cells.slice(), ..RunOptions::new(grid) });
            let mut count = 0;
            while it.next().unwrap().is_some() {
                count += 1;
            }
            assert_eq!(1, count);
        }

        // Spaces should be part of a run
        {
            let cells = terminal_row(10, 3, &[b"ABCD   EFG"], 0);

            let grid: &SharedGrid = &testdata.grid;
            let mut it = testdata.shaper.run_iterator(RunOptions { cells: cells.slice(), ..RunOptions::new(grid) });
            let mut count = 0;
            while it.next().unwrap().is_some() {
                count += 1;
            }
            assert_eq!(1, count);
        }

        {
            // Make a screen with some data
            let cells = terminal_row(5, 3, &["A😃D".as_bytes()], 0);

            // Get our run iterator
            let grid: &SharedGrid = &testdata.grid;
            let mut it = testdata.shaper.run_iterator(RunOptions { cells: cells.slice(), ..RunOptions::new(grid) });
            let mut count = 0;
            while it.next().unwrap().is_some() {
                count += 1;

                // All runs should be exactly length 1
                assert_eq!(1, it.hooks.shaper.hb_buf.get_length());
            }
            assert_eq!(3, count);
        }
    }

    #[test]
    fn run_iterator_empty_cells_with_background_set() {
        let mut testdata = test_shaper();

        {
            // Make a screen with some data
            let mut t = new_terminal(5, 3);
            {
                let mut s = t.vt_stream();
                // Set red background and write A
                s.next_slice(b"\x1b[48;2;255;0;0mA");
            }

            // Get our first row
            for x in [1, 2] {
                let list_cell = t
                    .screens
                    .active
                    .pages
                    .get_cell(crate::terminal::point::Point::Active(crate::terminal::point::Coordinate { x, y: 0 }))
                    .unwrap();
                let mut cell = page::Cell::default();
                cell.set_content_tag(page::ContentTag::BgColorRgb);
                cell.set_content_color_rgb(crate::terminal::color::Rgb { r: 0xFF, g: 0, b: 0 });
                // SAFETY: the list cell points at a live cell.
                unsafe { *list_cell.cell = cell };
            }

            let mut state = crate::terminal::RenderState::empty();
            state.update(&mut t);
            let cells = row_cells(&state, 0);

            // Get our run iterator
            let grid: &SharedGrid = &testdata.grid;
            let mut it = testdata.shaper.run_iterator(RunOptions { cells: cells.slice(), ..RunOptions::new(grid) });
            {
                let run = it.next().unwrap().unwrap();
                assert_eq!(3, it.hooks.shaper.hb_buf.get_length());
                let cells = it.hooks.shaper.shape(run).unwrap();
                assert_eq!(3, cells.len());
            }
            assert!(it.next().unwrap().is_none());
        }
    }

    #[test]
    fn shape() {
        let mut testdata = test_shaper();

        let mut buf = String::new();
        buf.push(char::from_u32(0x1F44D).unwrap()); // Thumbs up plain
        buf.push(char::from_u32(0x1F44D).unwrap()); // Thumbs up plain
        buf.push(char::from_u32(0x1F3FD).unwrap()); // Medium skin tone

        // Make a screen with some data
        let cells = terminal_row(10, 3, &[buf.as_bytes()], 0);

        // Get our run iterator
        let grid: &SharedGrid = &testdata.grid;
        let mut it = testdata.shaper.run_iterator(RunOptions { cells: cells.slice(), ..RunOptions::new(grid) });
        let mut count = 0;
        while let Some(run) = it.next().unwrap() {
            count += 1;
            assert_eq!(3, it.hooks.shaper.hb_buf.get_length());
            it.hooks.shaper.shape(run).unwrap();
        }
        assert_eq!(1, count);
    }

    /// Shared body of the ligature tests: every run has `run_cells` cells
    /// and shapes into one glyph; there is exactly one run.
    fn expect_single_lig(testdata: &mut TestShaper, input: &[u8], run_cells: u16) {
        let cells = terminal_row(5, 3, &[input], 0);

        let grid: &SharedGrid = &testdata.grid;
        let mut it = testdata.shaper.run_iterator(RunOptions { cells: cells.slice(), ..RunOptions::new(grid) });
        let mut count = 0;
        while let Some(run) = it.next().unwrap() {
            count += 1;

            assert_eq!(run_cells, run.cells);

            let cells = it.hooks.shaper.shape(run).unwrap();
            assert_eq!(1, cells.len());
        }
        assert_eq!(1, count);
    }

    #[test]
    fn shape_inconsolata_ligs() {
        let mut testdata = test_shaper();

        expect_single_lig(&mut testdata, b">=", 2);
        expect_single_lig(&mut testdata, b"===", 3);
    }

    #[test]
    fn shape_monaspace_ligs() {
        let mut testdata = test_shaper_with_font(TestFont::MonaspaceNeon);

        expect_single_lig(&mut testdata, b"===", 3);
    }

    // Ghostty doesn't currently support RTL and our renderers assume
    // that cells are in strict LTR order. This means that we need to
    // force RTL text to be LTR for rendering. This test ensures that
    // we are correctly forcing RTL text to be LTR.
    #[test]
    fn shape_arabic_forced_ltr() {
        let mut testdata = test_shaper_with_font(TestFont::Arabic);

        let cells = terminal_row(120, 30, &[include_bytes!("testdata/arabic.txt")], 0);

        let grid: &SharedGrid = &testdata.grid;
        let mut it = testdata.shaper.run_iterator(RunOptions { cells: cells.slice(), ..RunOptions::new(grid) });
        let mut count = 0;
        while let Some(run) = it.next().unwrap() {
            count += 1;
            assert_eq!(25, run.cells);

            let cells = it.hooks.shaper.shape(run).unwrap();
            assert_eq!(25, cells.len());

            let mut x: u16 = cells[0].x;
            for cell in &cells[1..] {
                assert_eq!(x + 1, cell.x);
                x = cell.x;
            }
        }
        assert_eq!(1, count);
    }

    #[test]
    fn shape_emoji_width() {
        let mut testdata = test_shaper();

        {
            let cells = terminal_row(5, 3, &["👍".as_bytes()], 0);

            let grid: &SharedGrid = &testdata.grid;
            let mut it = testdata.shaper.run_iterator(RunOptions { cells: cells.slice(), ..RunOptions::new(grid) });
            let mut count = 0;
            while let Some(run) = it.next().unwrap() {
                count += 1;

                assert_eq!(2, run.cells);

                let cells = it.hooks.shaper.shape(run).unwrap();
                assert_eq!(1, cells.len());
            }
            assert_eq!(1, count);
        }
    }

    #[test]
    fn shape_emoji_width_long() {
        let mut testdata = test_shaper();

        // Make a screen and add a long emoji sequence to it.
        let mut t = new_terminal(30, 3);

        {
            // SAFETY: the first node is live while `t` is.
            let page = unsafe { t.screens.active.pages.pages.first.unwrap().as_mut().page() };
            // SAFETY: row 1 and its first cell are live page memory.
            let row = unsafe { &mut *page.get_row(1) };
            let cell = unsafe { &mut (*page.get_cells(row))[0] };
            *cell = page::Cell::init(0x1F9D4); // Person with beard
            let graphemes: [u32; 4] = [
                0x1F3FB, // Light skin tone (Fitz 1-2)
                0x200D,  // ZWJ
                0x2642,  // Male sign
                0xFE0F,  // Emoji presentation selector
            ];
            page.set_graphemes(row, cell, &graphemes).unwrap();
        }

        let mut state = crate::terminal::RenderState::empty();
        state.update(&mut t);
        let cells = row_cells(&state, 1);

        // Get our run iterator
        let grid: &SharedGrid = &testdata.grid;
        let mut it = testdata.shaper.run_iterator(RunOptions { cells: cells.slice(), ..RunOptions::new(grid) });
        let mut count = 0;
        while let Some(run) = it.next().unwrap() {
            count += 1;
            assert_eq!(4, it.hooks.shaper.hb_buf.get_length());

            let cells = it.hooks.shaper.shape(run).unwrap();

            assert_eq!(1, cells.len());
        }
        assert_eq!(1, count);
    }

    #[test]
    fn shaper_selects_font_for_entire_grapheme() {
        let mut testdata = test_shaper();

        // Combining enclosing keycap
        let cells = terminal_row(5, 3, &["#\u{20E3}".as_bytes()], 0);

        let primary = testdata.grid.get_index('#' as u32, Style::Regular, None).unwrap().unwrap();
        assert!(testdata.grid.has_codepoint(primary, '#' as u32, None));
        assert!(!testdata.grid.has_codepoint(primary, 0x20E3, None));

        let additional = testdata.grid.get_index(0x20E3, Style::Regular, None).unwrap().unwrap();
        assert!(testdata.grid.has_codepoint(additional, '#' as u32, None));
        assert!(testdata.grid.has_codepoint(additional, 0x20E3, None));

        let grid: &SharedGrid = &testdata.grid;
        let mut it = testdata.shaper.run_iterator(RunOptions { cells: cells.slice(), ..RunOptions::new(grid) });
        let run = it.next().unwrap().unwrap();
        assert_eq!(additional, run.font_index);
    }

    /// Shared body of the variation selector tests.
    fn expect_variation_selector(selector: u32) {
        let mut testdata = test_shaper();

        let mut buf = String::new();
        buf.push(char::from_u32(0x270C).unwrap()); // Victory sign (default text)
        buf.push(char::from_u32(selector).unwrap()); // ZWJ to force text/color

        // Make a screen with some data
        let cells = terminal_row(10, 3, &[buf.as_bytes()], 0);

        // Get our run iterator
        let grid: &SharedGrid = &testdata.grid;
        let mut it = testdata.shaper.run_iterator(RunOptions { cells: cells.slice(), ..RunOptions::new(grid) });
        let mut count = 0;
        while let Some(run) = it.next().unwrap() {
            count += 1;
            assert_eq!(1, it.hooks.shaper.hb_buf.get_length());

            let cells = it.hooks.shaper.shape(run).unwrap();
            assert_eq!(1, cells.len());
        }
        assert_eq!(1, count);
    }

    #[test]
    fn shape_variation_selector_vs15() {
        expect_variation_selector(0xFE0E);
    }

    #[test]
    fn shape_variation_selector_vs16() {
        expect_variation_selector(0xFE0F);
    }

    #[test]
    fn shape_with_empty_cells_in_between() {
        let mut testdata = test_shaper();

        // Make a screen with some data
        let cells = terminal_row(30, 3, &[b"A", b"\x1b[5C", b"B"], 0);

        // Get our run iterator
        let grid: &SharedGrid = &testdata.grid;
        let mut it = testdata.shaper.run_iterator(RunOptions { cells: cells.slice(), ..RunOptions::new(grid) });
        let mut count = 0;
        while let Some(run) = it.next().unwrap() {
            count += 1;

            let cells = it.hooks.shaper.shape(run).unwrap();
            assert_eq!(1, count);
            assert_eq!(7, cells.len());
        }
    }

    #[test]
    fn shape_combining_characters() {
        let mut testdata = test_shaper();

        let mut buf = String::new();
        buf.push('n'); // Combining
        buf.push(char::from_u32(0x0308).unwrap()); // Combining
        buf.push(char::from_u32(0x0308).unwrap());
        buf.push('a');

        // Make a screen with some data
        let cells = terminal_row(30, 3, &[buf.as_bytes()], 0);

        // Get our run iterator
        let grid: &SharedGrid = &testdata.grid;
        let mut it = testdata.shaper.run_iterator(RunOptions { cells: cells.slice(), ..RunOptions::new(grid) });
        let mut count = 0;
        while let Some(run) = it.next().unwrap() {
            count += 1;

            let cells = it.hooks.shaper.shape(run).unwrap();
            assert_eq!(4, cells.len());
            assert_eq!(0, cells[0].x);
            assert_eq!(0, cells[1].x);
            assert_eq!(0, cells[2].x);
            assert_eq!(1, cells[3].x);
        }
        assert_eq!(1, count);
    }

    /// Zig: a terminal with grapheme clustering set to `grapheme_cluster`
    /// fed with `input`, returning row 0.
    fn terminal_row_grapheme(grapheme_cluster: bool, input: &str) -> Cells {
        let mut t = new_terminal(30, 3);

        t.modes.set(crate::terminal::modes::Mode::GraphemeCluster, grapheme_cluster);

        {
            let mut s = t.vt_stream();
            s.next_slice(input.as_bytes());
        }

        let mut state = crate::terminal::RenderState::empty();
        state.update(&mut t);
        row_cells(&state, 0)
    }

    // This test exists because the string it uses causes HarfBuzz to output a
    // non-monotonic run with our cluster level set to `characters`, which we need
    // to handle by tracking the max cluster for the run.
    #[test]
    fn shape_devanagari_string() {
        // We need a font that supports devanagari for this to work, if we can't
        // find Arial Unicode MS, which is a system font on macOS, we just skip
        // the test.
        let Some(mut testdata) = test_shaper_with_discovered_font("Arial Unicode MS") else {
            return;
        };

        // Disable grapheme clustering
        let cells = terminal_row_grapheme(false, "अपार्टमेंट");

        // Get our run iterator
        let grid: &SharedGrid = &testdata.grid;
        let mut it = testdata.shaper.run_iterator(RunOptions { cells: cells.slice(), ..RunOptions::new(grid) });

        let run = it.next().unwrap();
        assert!(run.is_some());
        {
            let cells = it.hooks.shaper.shape(run.unwrap()).unwrap();

            assert_eq!(8, cells.len());
            assert_eq!(0, cells[0].x);
            assert_eq!(1, cells[1].x);
            assert_eq!(2, cells[2].x);
            assert_eq!(4, cells[3].x);
            assert_eq!(4, cells[4].x);
            assert_eq!(5, cells[5].x);
            assert_eq!(5, cells[6].x);
            assert_eq!(6, cells[7].x);
        }

        assert!(it.next().unwrap().is_none());
    }

    // This test fails on Linux if you have the "Noto Sans Tai Tham" font installed
    // locally. Disabling this test until it can be fixed.
    #[test]
    fn shape_tai_tham_vowels_position_differs_from_advance() {
        // return error.SkipZigTest; (the Zig body is commented out)
    }

    #[test]
    fn shape_tibetan_characters() {
        // We need a font that has multiple glyphs for this codepoint to reproduce
        // the old broken behavior, and Noto Serif Tibetan is one of them. It's not
        // a default Mac font, and if we can't find it we just skip the test.
        let Some(mut testdata) = test_shaper_with_discovered_font("Noto Serif Tibetan") else {
            return;
        };

        // Enable grapheme clustering
        let cells = terminal_row_grapheme(true, "\u{0f00}"); // ༀ

        // Get our run iterator
        let grid: &SharedGrid = &testdata.grid;
        let mut it = testdata.shaper.run_iterator(RunOptions { cells: cells.slice(), ..RunOptions::new(grid) });
        let mut count = 0;
        while let Some(run) = it.next().unwrap() {
            count += 1;

            let cells = it.hooks.shaper.shape(run).unwrap();
            assert_eq!(2, cells.len());
            assert_eq!(0, cells[0].x);
            assert_eq!(0, cells[1].x);

            // The second glyph renders at the correct location
            assert!(cells[1].x_offset < 2);
        }
        assert_eq!(1, count);
    }

    // This test fails on Linux if you have the "Noto Sans Tai Tham" font installed
    // locally. Disabling this test until it can be fixed.
    #[test]
    fn shape_tai_tham_letters_run_offset_y_differs_from_zero() {
        // return error.SkipZigTest; (the Zig body is commented out)
    }

    // This test fails on Linux if you have the "Noto Sans Javanese" font installed
    // locally. Disabling this test until it can be fixed.
    #[test]
    fn shape_javanese_ligatures() {
        // return error.SkipZigTest; (the Zig body is commented out)
    }

    #[test]
    fn shape_chakma_vowel_sign_with_ligature_vowel_sign_renders_first() {
        // We need a font that supports Chakma for this to work, if we can't find
        // Noto Sans Chakma Regular, which is a system font on macOS, we just skip
        // the test.
        let Some(mut testdata) = test_shaper_with_discovered_font("Noto Sans Chakma") else {
            return;
        };

        let mut buf = String::new();
        // First grapheme cluster:
        buf.push(char::from_u32(0x1111d).unwrap()); // BAA
        // Second grapheme cluster:
        buf.push(char::from_u32(0x11116).unwrap()); // TAA
        buf.push(char::from_u32(0x11133).unwrap()); // Virama
        // Third grapheme cluster, combining with the second in a ligature:
        buf.push(char::from_u32(0x11120).unwrap()); // YYAA
        buf.push(char::from_u32(0x1112c).unwrap()); // Vowel Sign U

        // Enable grapheme clustering
        let cells = terminal_row_grapheme(true, &buf);

        // Get our run iterator
        let grid: &SharedGrid = &testdata.grid;
        let mut it = testdata.shaper.run_iterator(RunOptions { cells: cells.slice(), ..RunOptions::new(grid) });
        let mut count = 0;
        while let Some(run) = it.next().unwrap() {
            count += 1;

            let cells = it.hooks.shaper.shape(run).unwrap();
            assert_eq!(4, cells.len());
            assert_eq!(0, cells[0].x);
            // See the giant "We need to reset the `cell_offset`" comment, but here
            // we should technically have the rest of these be `x` of 1, but that
            // would require going back in the stream to adjust past cells, and
            // we don't take on that complexity.
            assert_eq!(0, cells[1].x);
            assert_eq!(0, cells[2].x);
            assert_eq!(0, cells[3].x);

            // The vowel sign U renders before the TAA:
            assert!(cells[1].x_offset < cells[2].x_offset);
        }
        assert_eq!(1, count);
    }

    #[test]
    fn shape_bengali_ligatures_with_out_of_order_vowels() {
        // Whereas this test in CoreText had everything shaping into one giant
        // ligature, HarfBuzz splits it into a few clusters. It still looks okay
        // (see #10332).

        // We need a font that supports Bengali for this to work, if we can't find
        // Arial Unicode MS, which is a system font on macOS, we just skip the
        // test.
        let Some(mut testdata) = test_shaper_with_discovered_font("Arial Unicode MS") else {
            return;
        };

        let mut buf = String::new();
        // First grapheme cluster:
        buf.push(char::from_u32(0x09b0).unwrap()); // RA
        buf.push(char::from_u32(0x09be).unwrap()); // Vowel sign AA
        // Second grapheme cluster:
        buf.push(char::from_u32(0x09b7).unwrap()); // SSA
        buf.push(char::from_u32(0x09cd).unwrap()); // Virama
        // Third grapheme cluster:
        buf.push(char::from_u32(0x099f).unwrap()); // TTA
        buf.push(char::from_u32(0x09cd).unwrap()); // Virama
        // Fourth grapheme cluster:
        buf.push(char::from_u32(0x09b0).unwrap()); // RA
        buf.push(char::from_u32(0x09c7).unwrap()); // Vowel sign E

        // Enable grapheme clustering
        let cells = terminal_row_grapheme(true, &buf);

        // Get our run iterator
        let grid: &SharedGrid = &testdata.grid;
        let mut it = testdata.shaper.run_iterator(RunOptions { cells: cells.slice(), ..RunOptions::new(grid) });
        let mut count = 0;
        while let Some(run) = it.next().unwrap() {
            count += 1;

            let cells = it.hooks.shaper.shape(run).unwrap();
            assert_eq!(8, cells.len());
            assert_eq!(0, cells[0].x);
            assert_eq!(0, cells[1].x);

            // Whereas CoreText puts everything all into the first cell (see the
            // corresponding test), HarfBuzz splits into two clusters.
            assert_eq!(2, cells[2].x);
            assert_eq!(2, cells[3].x);
            assert_eq!(2, cells[4].x);
            assert_eq!(2, cells[5].x);
            assert_eq!(2, cells[6].x);
            assert_eq!(2, cells[7].x);

            // The vowel sign E renders before the SSA:
            assert!(cells[2].x_offset < cells[3].x_offset);
        }
        assert_eq!(1, count);
    }

    #[test]
    fn shape_box_glyphs() {
        let mut testdata = test_shaper();

        let mut buf = String::new();
        buf.push(char::from_u32(0x2500).unwrap()); // horiz line
        buf.push(char::from_u32(0x2501).unwrap()); //

        // Make a screen with some data
        let cells = terminal_row(10, 3, &[buf.as_bytes()], 0);

        // Get our run iterator
        let grid: &SharedGrid = &testdata.grid;
        let mut it = testdata.shaper.run_iterator(RunOptions { cells: cells.slice(), ..RunOptions::new(grid) });
        let mut count = 0;
        while let Some(run) = it.next().unwrap() {
            count += 1;
            assert_eq!(2, it.hooks.shaper.hb_buf.get_length());
            let cells = it.hooks.shaper.shape(run).unwrap();
            assert_eq!(2, cells.len());
            assert_eq!(0x2500, cells[0].glyph_index);
            assert_eq!(0, cells[0].x);
            assert_eq!(0x2501, cells[1].glyph_index);
            assert_eq!(1, cells[1].x);
        }
        assert_eq!(1, count);
    }

    #[test]
    fn shape_selection_boundary() {
        let mut testdata = test_shaper();

        // Make a screen with some data
        let cells = terminal_row(10, 3, &[b"a1b2c3d4e5"], 0);

        // Full line selection
        assert_eq!(1, count_runs(&mut testdata, |o| RunOptions { selection: Some([0, 9]), ..o }, &cells));

        // Offset x, goes to end of line selection
        assert_eq!(2, count_runs(&mut testdata, |o| RunOptions { selection: Some([2, 9]), ..o }, &cells));

        // Offset x, starts at beginning of line
        assert_eq!(2, count_runs(&mut testdata, |o| RunOptions { selection: Some([0, 3]), ..o }, &cells));

        // Selection only subset of line
        assert_eq!(3, count_runs(&mut testdata, |o| RunOptions { selection: Some([1, 3]), ..o }, &cells));

        // Selection only one character
        assert_eq!(3, count_runs(&mut testdata, |o| RunOptions { selection: Some([1, 1]), ..o }, &cells));
    }

    #[test]
    fn shape_cursor_boundary() {
        let mut testdata = test_shaper();

        // Make a screen with some data
        let cells = terminal_row(10, 3, &[b"a1b2c3d4e5"], 0);

        // No cursor is full line
        assert_eq!(1, count_runs(&mut testdata, |o| o, &cells));

        {
            // Cursor at index 0 is two runs
            assert_eq!(2, count_runs(&mut testdata, |o| RunOptions { cursor_x: Some(0), ..o }, &cells));
            // And without cursor splitting remains one
            assert_eq!(1, count_runs(&mut testdata, |o| o, &cells));
        }

        {
            // Cursor at index 1 is three runs
            assert_eq!(3, count_runs(&mut testdata, |o| RunOptions { cursor_x: Some(1), ..o }, &cells));
            // And without cursor splitting remains one
            assert_eq!(1, count_runs(&mut testdata, |o| o, &cells));
        }
        {
            // Cursor at last col is two runs
            assert_eq!(2, count_runs(&mut testdata, |o| RunOptions { cursor_x: Some(9), ..o }, &cells));
            // And without cursor splitting remains one
            assert_eq!(1, count_runs(&mut testdata, |o| o, &cells));
        }
    }

    #[test]
    fn shape_cursor_boundary_and_colored_emoji() {
        let mut testdata = test_shaper();

        // Make a screen with some data
        let cells = terminal_row(3, 10, &["👍🏼".as_bytes()], 0);

        // No cursor is full line
        assert_eq!(1, count_runs(&mut testdata, |o| o, &cells));

        // Cursor on emoji does not split it
        assert_eq!(1, count_runs(&mut testdata, |o| RunOptions { cursor_x: Some(0), ..o }, &cells));
        assert_eq!(1, count_runs(&mut testdata, |o| o, &cells));
        assert_eq!(1, count_runs(&mut testdata, |o| RunOptions { cursor_x: Some(1), ..o }, &cells));
        assert_eq!(1, count_runs(&mut testdata, |o| o, &cells));
    }

    #[test]
    fn shape_cell_attribute_change() {
        let mut testdata = test_shaper();

        // Plain >= should shape into 1 run
        {
            let cells = terminal_row(10, 3, &[b">="], 0);
            assert_eq!(1, count_runs(&mut testdata, |o| o, &cells));
        }

        // Bold vs regular should split
        {
            let cells = terminal_row(3, 10, &[b">", b"\x1b[1m", b"="], 0);
            assert_eq!(2, count_runs(&mut testdata, |o| o, &cells));
        }

        // Changing fg color should split
        {
            let cells = terminal_row(
                3,
                10,
                &[
                    // RGB 1, 2, 3
                    b"\x1b[38;2;1;2;3m",
                    b">",
                    // RGB 3, 2, 1
                    b"\x1b[38;2;3;2;1m",
                    b"=",
                ],
                0,
            );
            assert_eq!(2, count_runs(&mut testdata, |o| o, &cells));
        }

        // Changing bg color should not split
        {
            let cells = terminal_row(
                3,
                10,
                &[
                    // RGB 1, 2, 3 bg
                    b"\x1b[48;2;1;2;3m",
                    b">",
                    // RGB 3, 2, 1 bg
                    b"\x1b[48;2;3;2;1m",
                    b"=",
                ],
                0,
            );
            assert_eq!(1, count_runs(&mut testdata, |o| o, &cells));
        }

        // Same bg color should not split
        {
            let cells = terminal_row(
                3,
                10,
                &[
                    // RGB 1, 2, 3 bg
                    b"\x1b[48;2;1;2;3m",
                    b">",
                    b"=",
                ],
                0,
            );
            assert_eq!(1, count_runs(&mut testdata, |o| o, &cells));
        }
    }
}
