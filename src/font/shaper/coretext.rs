//! Port of `src/font/shaper/coretext.zig`.
//!
//! STATUS: ported (macOS-only; type-checked with
//! `cargo check --target x86_64-apple-darwin`, tests not run on Linux)
//!
//! The Zig tests build `terminal.Terminal` and `terminal.RenderState`
//! instances and feed them through `Terminal::vt_stream`.

#![cfg(any(target_os = "macos", target_os = "ios"))]

use std::ffi::c_void;

use crate::font::collection::{GetFaceError, Index};
use crate::font::shape::{self, Cell, Feature, FeatureList, RunOptions, TextRun, DEFAULT_FEATURES};
use crate::font::shaper::run::{self, RunIterator};
use crate::font::shared_grid::SharedGrid;
use crate::os::cf_release_thread::{self, CFTypeRef};
use crate::pkg::macos;
use crate::quirks::inline_assert as assert;

const LOG: &str = "font_shaper";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Codepoint {
    pub codepoint: u32,
    pub cluster: u32,
}

#[derive(Debug, Default)]
pub struct RunState {
    pub codepoints: Vec<Codepoint>,
    pub unichars: Vec<u16>,
}

impl RunState {
    fn init() -> RunState {
        RunState::default()
    }

    fn reset(&mut self) {
        self.codepoints.clear();
        self.unichars.clear();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct Offset {
    cluster: u32,
    x: f64,
}

/// Errors from the CoreText shaper (the Zig inferred error sets).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Macos(macos::Error),
    GetFace(GetFaceError),
    ThreadSpawnFailed,
    Xev(crate::pkg::xev::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for Error {}

impl From<macos::Error> for Error {
    fn from(e: macos::Error) -> Error {
        Error::Macos(e)
    }
}

impl From<GetFaceError> for Error {
    fn from(e: GetFaceError) -> Error {
        Error::GetFace(e)
    }
}

/// A CoreFoundation object as a raw reference for the release pool.
fn cf<T: macos::CFType>(v: &T) -> CFTypeRef {
    CFTypeRef(v.as_ptr() as *const c_void)
}

/// Shaper that uses CoreText.
///
/// CoreText shaping differs in subtle ways from HarfBuzz so it may result
/// in inconsistent rendering across platforms. But it also fixes many
/// issues (some macOS specific):
///
///   - Theta hat offset is incorrect in HarfBuzz but correct by default
///     on macOS applications using CoreText. (See:
///     https://github.com/harfbuzz/harfbuzz/discussions/4525)
///
///   - Hyphens (U+2010) can be synthesized by CoreText but not by HarfBuzz.
///     See: https://github.com/mitchellh/ghostty/issues/1643
///
pub struct Shaper {
    /// The string used for shaping the current run.
    pub run_state: RunState,

    /// CoreFoundation Dictionary which represents our font feature settings.
    features: &'static macos::foundation::Dictionary,
    /// A version of the features dictionary with the default features excluded.
    features_no_default: &'static macos::foundation::Dictionary,

    /// The shared memory used for shaping results.
    cell_buf: Vec<Cell>,

    /// Cached attributes dict for creating CTTypesetter objects.
    /// The values in this never change so we can avoid overhead
    /// by just creating it once and saving it for reuse.
    typesetter_attr_dict: &'static macos::foundation::Dictionary,

    /// List where we cache fonts, so we don't have to remake them for
    /// every single shaping operation.
    ///
    /// Fonts are cached as attribute dictionaries to be applied directly to
    /// attributed strings.
    cached_fonts: Vec<Option<&'static macos::foundation::Dictionary>>,

    /// The grid that our cached fonts correspond to.
    /// If the grid changes then we need to reset our cache.
    cached_font_grid: usize,

    /// The list of CoreFoundation objects to release on the dedicated
    /// release thread. This is built up over the course of shaping and
    /// sent to the release thread when endFrame is called.
    cf_release_pool: Vec<CFTypeRef>,

    /// Dedicated thread for releasing CoreFoundation objects. Some objects,
    /// such as those produced by CoreText, have excessively slow release
    /// callback logic.
    cf_release_thread: Box<cf_release_thread::Thread>,
    cf_release_thr: Option<std::thread::JoinHandle<()>>,
}

impl Shaper {
    /// Create a CoreFoundation Dictionary suitable for
    /// settings the font features of a CoreText font.
    fn make_features_dict(feats: &[Feature]) -> Result<&'static macos::foundation::Dictionary, Error> {
        let list = macos::foundation::MutableArray::create()?;
        // The list will be retained by the dict once we add it to it.

        let result = (|| -> Result<&'static macos::foundation::Dictionary, Error> {
            for feat in feats {
                let value_num: std::ffi::c_int = feat.value as std::ffi::c_int;

                // Keys can only be ASCII.
                let key = macos::foundation::String::create_with_bytes(
                    &feat.tag,
                    macos::foundation::StringEncoding::Ascii,
                    false,
                )?;
                let value = macos::foundation::Number::create(macos::foundation::NumberType::Int, &value_num);
                let value = match value {
                    Ok(v) => v,
                    Err(e) => {
                        key.release();
                        return Err(e.into());
                    }
                };

                // SAFETY: reading framework constants.
                let keys = unsafe {
                    [
                        macos::text::c::kCTFontOpenTypeFeatureTag,
                        macos::text::c::kCTFontOpenTypeFeatureValue,
                    ]
                };
                let dict = macos::foundation::Dictionary::create(
                    Some(&keys),
                    Some(&[key.as_ptr() as *const c_void, value.as_ptr() as *const c_void]),
                );
                key.release();
                value.release();
                let dict = dict?;

                list.append_value(dict);
                dict.release();
            }

            // SAFETY: reading a framework constant.
            let keys = unsafe { [macos::text::c::kCTFontFeatureSettingsAttribute] };
            let dict = macos::foundation::Dictionary::create(
                Some(&keys),
                Some(&[list.as_ptr() as *const c_void]),
            )?;

            Ok(dict)
        })();
        list.release();
        result
    }

    /// The cell_buf argument is the buffer to use for storing shaped results.
    /// This should be at least the number of columns in the terminal.
    pub fn init(opts: shape::Options<'_>) -> Result<Shaper, Error> {
        let mut feature_list = FeatureList::default();
        for feature_str in opts.features {
            feature_list.append_from_string(feature_str.as_bytes());
        }

        // We need to construct two attrs dictionaries for font features;
        // one without the default features included, and one with them.
        let feats = &feature_list.features;
        let mut feats_df: Vec<Feature> = Vec::with_capacity(feats.len() + DEFAULT_FEATURES.len());
        feats_df.extend_from_slice(&DEFAULT_FEATURES);
        feats_df.extend_from_slice(feats);

        let features = Self::make_features_dict(&feats_df)?;
        let features_no_default = match Self::make_features_dict(feats) {
            Ok(v) => v,
            Err(e) => {
                features.release();
                return Err(e);
            }
        };

        let run_state = RunState::init();

        // For now we only support LTR text. If we shape RTL text then
        // rendering will be very wrong so we need to explicitly force
        // LTR no matter what.
        //
        // See: https://github.com/mitchellh/ghostty/issues/1737
        // See: https://github.com/mitchellh/ghostty/issues/1442
        //
        // We used to do this by setting the writing direction attribute
        // on the attributed string we used, but it seems like that will
        // still allow some weird results, for example a single space at
        // the end of a line composed of RTL characters will be cause it
        // to output a run containing just that space, BEFORE it outputs
        // the rest of the line as a separate run, very weirdly with the
        // "right to left" flag set in the single space run's run status...
        //
        // So instead what we do is use a CTTypesetter to create our line,
        // using the kCTTypesetterOptionForcedEmbeddingLevel attribute to
        // force CoreText not to try doing any sort of BiDi, instead just
        // treat all text as embedding level 0 (left to right).
        let typesetter_attr_dict = (|| -> Result<&'static macos::foundation::Dictionary, Error> {
            let num = macos::foundation::Number::create(macos::foundation::NumberType::Int, &(0 as std::ffi::c_int))?;
            // SAFETY: reading a framework constant.
            let keys = unsafe { [macos::c::kCTTypesetterOptionForcedEmbeddingLevel] };
            let dict = macos::foundation::Dictionary::create(
                Some(&keys),
                Some(&[num.as_ptr() as *const c_void]),
            );
            num.release();
            Ok(dict?)
        })();
        let typesetter_attr_dict = match typesetter_attr_dict {
            Ok(v) => v,
            Err(e) => {
                features.release();
                features_no_default.release();
                return Err(e);
            }
        };

        // Create the CF release thread.
        let mut cf_release_thread = match cf_release_thread::Thread::init() {
            Ok(v) => Box::new(v),
            Err(err) => {
                features.release();
                features_no_default.release();
                typesetter_attr_dict.release();
                return Err(Error::Xev(err));
            }
        };

        // Start the CF release thread.
        struct ThreadPtr(*mut cf_release_thread::Thread);
        // SAFETY: the boxed thread outlives the OS thread (joined on drop).
        unsafe impl Send for ThreadPtr {}
        let thr_state = ThreadPtr(&mut *cf_release_thread);
        let cf_release_thr = std::thread::Builder::new()
            .name("cf_release".to_string())
            .spawn(move || {
                let thr_state = thr_state;
                // SAFETY: the boxed thread does not move and is only freed
                // after this thread is joined.
                unsafe { cf_release_thread::Thread::thread_main(thr_state.0) }
            });
        let cf_release_thr = match cf_release_thr {
            Ok(v) => v,
            Err(_) => {
                cf_release_thread.deinit();
                features.release();
                features_no_default.release();
                typesetter_attr_dict.release();
                return Err(Error::ThreadSpawnFailed);
            }
        };

        Ok(Shaper {
            cell_buf: Vec::new(),
            run_state,
            features,
            features_no_default,
            typesetter_attr_dict,
            cached_fonts: Vec::new(),
            cached_font_grid: 0,
            cf_release_pool: Vec::new(),
            cf_release_thread,
            cf_release_thr: Some(cf_release_thr),
        })
    }

    pub fn end_frame(&mut self) {
        if self.cf_release_pool.is_empty() {
            return;
        }

        // Get all the items in the pool as an owned slice so we can
        // send it to the dedicated release thread.
        let items = std::mem::take(&mut self.cf_release_pool);

        // Send the items. If the send succeeds then we wake up the
        // thread to process the items. If the send fails then do a manual
        // cleanup.
        let pushed = self.cf_release_thread.mailbox.push(
            cf_release_thread::Message::Release { refs: items.clone() },
            crate::datastruct::blocking_queue::Timeout::Forever,
        );
        if pushed != 0 {
            if let Err(err) = self.cf_release_thread.wakeup.notify() {
                log::warn!(
                    target: LOG,
                    "error notifying cf release thread to wake up, may stall err={err}"
                );
            }
            return;
        }

        for r in items {
            // SAFETY: each pooled reference is owned by the pool.
            unsafe { macos::foundation::cf_release(r.0 as *mut c_void) };
        }
    }

    /// Returns an iterator that returns one text run at a time for the
    /// given terminal row. The shaper is reachable while iterating as
    /// `it.hooks.shaper`.
    pub fn run_iterator<'a>(&'a mut self, opts: RunOptions<'a>) -> RunIterator<'a, RunIteratorHook<'a>> {
        RunIterator::new(RunIteratorHook { shaper: self }, opts)
    }

    /// Note that this will accumulate garbage in the release pool. The
    /// caller must ensure you're properly calling endFrame to release
    /// all the objects.
    pub fn shape(&mut self, run: TextRun<'_>) -> Result<&[Cell], Error> {
        // {
        //     log.debug("shape -----------------------------------", .{});
        //     for (state.codepoints.items) |entry| {
        //         log.debug("cp={X} cluster={}", .{ entry.codepoint, entry.cluster });
        //     }
        //     log.debug("shape end -------------------------------", .{});
        // }

        // Special fonts aren't shaped and their codepoint == glyph so we
        // can just return the codepoints as-is.
        if run.font_index.special().is_some() {
            self.cell_buf.clear();
            self.cell_buf.reserve(self.run_state.codepoints.len());
            for entry in &self.run_state.codepoints {
                // We use null codepoints to pad out our list so indices match
                // the UTF-16 string we constructed for CoreText. We don't want
                // to emit these if this is a special font, since they're not
                // part of the original run.
                if entry.codepoint == 0 {
                    continue;
                }

                self.cell_buf.push(Cell {
                    x: entry.cluster as u16,
                    glyph_index: entry.codepoint,
                    ..Default::default()
                });
            }

            return Ok(&self.cell_buf);
        }

        let attr_dict: &'static macos::foundation::Dictionary = self.get_font(run.grid, run.font_index)?;

        // Make room for the attributed string, CTTypesetter, and CTLine.
        self.cf_release_pool.reserve(4);

        let str = macos::foundation::String::create_with_characters_no_copy(&self.run_state.unichars)?;
        self.cf_release_pool.push(cf(str));

        // Create an attributed string from our string
        let attr_str = macos::foundation::AttributedString::create(str, attr_dict)?;
        self.cf_release_pool.push(cf(attr_str));

        // Create a typesetter from the attributed string and the cached
        // attr dict. (See comment in init for more info on the attr dict.)
        let typesetter =
            macos::text::Typesetter::create_with_attributed_string_and_options(attr_str, self.typesetter_attr_dict)?;
        self.cf_release_pool.push(cf(typesetter));

        // Create a line from the typesetter
        let line = typesetter.create_line(macos::foundation::Range { location: 0, length: 0 }.cval())?;
        self.cf_release_pool.push(cf(line));

        // This keeps track of the current x offset (sum of advance.width) and
        // the furthest cluster we've seen so far (max).
        let mut run_offset = Offset::default();

        // This keeps track of the cell starting x and cluster.
        let mut cell_offset = Offset::default();

        // For debugging positions, turn this on:
        //let mut run_offset_y: f64 = 0.0;
        //let mut cell_offset_y: f64 = 0.0;

        // Clear our cell buf and make sure we have enough room for the whole
        // line of glyphs, so that we can just assume capacity when appending
        // instead of maybe allocating.
        self.cell_buf.clear();
        self.cell_buf.reserve(line.get_glyph_count() as usize);

        // CoreText, despite our insistence with an enforced embedding level,
        // may sometimes output runs that are non-monotonic. In order to fix
        // this, we check the run status for each run and if any aren't ltr
        // we set this to true, which indicates that we must sort our buffer.
        let mut non_ltr: bool = false;

        // CoreText may generate multiple runs even though our input to
        // CoreText is already split into runs by our own run iterator.
        // The runs as far as I can tell are always sequential to each
        // other so we can iterate over them and just append to our
        // cell buffer.
        let runs = line.get_glyph_runs();
        for run_i in 0..runs.get_count() {
            // SAFETY: the glyph runs array holds CTRuns.
            let ctrun: &macos::text::Run = unsafe { runs.get_value_at_index(run_i) };

            let status = ctrun.get_status();
            if status.non_monotonic || status.right_to_left {
                non_ltr = true;
            }

            // Get our glyphs and positions
            use std::borrow::Cow;
            let glyphs: Cow<'_, [macos::graphics::Glyph]> = match ctrun.get_glyphs_ptr() {
                Some(v) => Cow::Borrowed(v),
                None => Cow::Owned(ctrun.get_glyphs()),
            };
            let advances: Cow<'_, [macos::graphics::Size]> = match ctrun.get_advances_ptr() {
                Some(v) => Cow::Borrowed(v),
                None => Cow::Owned(ctrun.get_advances()),
            };
            let positions: Cow<'_, [macos::graphics::Point]> = match ctrun.get_positions_ptr() {
                Some(v) => Cow::Borrowed(v),
                None => Cow::Owned(ctrun.get_positions()),
            };
            let indices: Cow<'_, [usize]> = match ctrun.get_string_indices_ptr() {
                Some(v) => Cow::Borrowed(v),
                None => Cow::Owned(ctrun.get_string_indices()),
            };
            assert(glyphs.len() == advances.len());
            assert(glyphs.len() == positions.len());
            assert(glyphs.len() == indices.len());

            for k in 0..glyphs.len() {
                let glyph = glyphs[k];
                let advance = advances[k];
                let position = positions[k];
                let index = indices[k] as usize;

                // Our cluster is also our cell X position. If the cluster changes
                // then we need to reset our current cell offsets.
                let cluster = self.run_state.codepoints[index].cluster;
                if cell_offset.cluster != cluster {
                    // We previously asserted that the new cluster is greater
                    // than cell_offset.cluster, but this isn't always true.
                    // See e.g. the "shape Chakma vowel sign with ligature
                    // (vowel sign renders first)" test.

                    let is_after_glyph_from_current_or_next_clusters = cluster <= run_offset.cluster;

                    let is_first_codepoint_in_cluster = 'blk: {
                        let mut i = index;
                        while i > 0 {
                            i -= 1;
                            let codepoint = self.run_state.codepoints[i];

                            // Skip surrogate pair padding
                            if codepoint.codepoint == 0 {
                                continue;
                            }
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
                        cell_offset = Offset {
                            cluster,
                            x: run_offset.x,
                        };

                        // For debugging positions, turn this on:
                        //cell_offset_y = run_offset_y;
                    }
                }

                // For debugging positions, turn this on:
                //self.debug_positions(run_offset, run_offset_y, cell_offset, cell_offset_y, position, index);

                let x_offset = position.x - cell_offset.x;

                self.cell_buf.push(Cell {
                    x: cell_offset.cluster as u16,
                    x_offset: x_offset.round() as i16,
                    y_offset: position.y.round() as i16,
                    glyph_index: glyph as u32,
                });

                // Add our advances to keep track of our run offsets.
                // Advances apply to the NEXT cell.
                run_offset.x += advance.width;
                run_offset.cluster = run_offset.cluster.max(cluster);

                // For debugging positions, turn this on:
                //run_offset_y += advance.height;
            }
        }

        // If our buffer contains some non-ltr sections we need to sort it :/
        if non_ltr {
            // This is EXCEPTIONALLY rare. Only happens for languages with
            // complex shaping which we don't even really support properly
            // right now, so are very unlikely to be used heavily by users
            // of Ghostty.
            //
            // NOTE: Zig's `std.mem.sort` is a stable sort, as is `sort_by`.
            self.cell_buf.sort_by(|a, b| a.x.cmp(&b.x));
        }

        Ok(&self.cell_buf)
    }

    /// Get an attr dict for a font from a specific index.
    /// These items are cached, do not retain or release them.
    fn get_font(&mut self, grid: &SharedGrid, index: Index) -> Result<&'static macos::foundation::Dictionary, Error> {
        // If this grid doesn't match the one we've cached fonts for,
        // then we reset the cache list since it's no longer valid.
        // We use an intFromPtr rather than direct pointer comparison
        // because we don't want anyone to inadvertently use the pointer.
        let grid_id: usize = grid as *const SharedGrid as usize;
        if grid_id != self.cached_font_grid {
            if self.cached_font_grid > 0 {
                // Put all the currently cached fonts in to
                // the release pool before clearing the list.
                self.cf_release_pool.reserve(self.cached_fonts.len());
                for f in self.cached_fonts.iter().flatten() {
                    self.cf_release_pool.push(cf(*f));
                }
                self.cached_fonts.clear();
            }

            self.cached_font_grid = grid_id;
        }

        let index_int = index.int() as usize;

        // The cached fonts are indexed directly by the font index, since
        // this number is usually low. Therefore, we set any index we haven't
        // seen to null.
        if self.cached_fonts.len() <= index_int {
            self.cached_fonts.resize_with(index_int + 1, || None);
        }

        // If we have it, return the cached attr dict.
        if let Some(cached) = self.cached_fonts[index_int] {
            return Ok(cached);
        }

        // Font descriptor, font
        self.cf_release_pool.reserve(2);

        let run_font = {
            // The CoreText shaper relies on CoreText and CoreText claims
            // that CTFonts are threadsafe. See:
            // https://developer.apple.com/documentation/coretext/
            //
            // Quote:
            // All individual functions in Core Text are thread-safe. Font
            // objects (CTFont, CTFontDescriptor, and associated objects) can
            // be used simultaneously by multiple operations, work queues, or
            // threads. However, the layout objects (CTTypesetter,
            // CTFramesetter, CTRun, CTLine, CTFrame, and associated objects)
            // should be used in a single operation, work queue, or thread.
            //
            // Because of this, we only acquire the lock to grab the
            // face and set it up, then release it. (The Zig code takes the
            // shared lock; `getFace` needs mutable access in Rust.)
            let mut state = grid.lock.write().unwrap_or_else(|e| e.into_inner());

            let face = state.resolver.collection.get_face(index)?;
            let original = face.font;

            let attrs = if face.quirks_disable_default_font_features {
                self.features_no_default
            } else {
                self.features
            };

            let desc = macos::text::FontDescriptor::create_with_attributes(attrs)?;
            self.cf_release_pool.push(cf(desc));

            original.copy_with_attributes(0.0, None, Some(desc))?
        };
        self.cf_release_pool.push(cf(run_font));

        // Get our font and use that get the attributes to set for the
        // attributed string so the whole string uses the same font.
        let attr_dict = macos::foundation::Dictionary::create(
            Some(&[macos::text::StringAttribute::Font.key().as_ptr() as *const c_void]),
            Some(&[run_font.as_ptr() as *const c_void]),
        )?;

        self.cached_fonts[index_int] = Some(attr_dict);
        Ok(attr_dict)
    }

    #[allow(dead_code, clippy::too_many_arguments)]
    fn debug_positions(
        &self,
        run_offset: Offset,
        run_offset_y: f64,
        cell_offset: Offset,
        cell_offset_y: f64,
        position: macos::graphics::Point,
        index: usize,
    ) {
        use std::fmt::Write as _;

        let state = &self.run_state;
        let x_offset = position.x - cell_offset.x;
        let advance_x_offset = run_offset.x - cell_offset.x;
        let advance_y_offset = run_offset_y - cell_offset_y;
        let x_offset_diff = x_offset - advance_x_offset;
        let y_offset_diff = position.y - advance_y_offset;
        let positions_differ = x_offset_diff.abs() > 0.0001 || y_offset_diff.abs() > 0.0001;
        let old_offset_y = position.y - cell_offset_y;
        let position_y_differs = cell_offset_y.abs() > 0.0001;
        let cluster = state.codepoints[index].cluster;
        let cluster_differs = cluster != cell_offset.cluster;

        // To debug every loop, flip this to true:
        let extra_debugging = false;

        let is_previous_codepoint_prepend = if cluster_differs || extra_debugging {
            'blk: {
                let mut i = index;
                while i > 0 {
                    i -= 1;
                    let codepoint = state.codepoints[i];

                    // Skip surrogate pair padding
                    if codepoint.codepoint == 0 {
                        continue;
                    }

                    break 'blk crate::unicode::TABLE.get(codepoint.codepoint).grapheme_break
                        == crate::unicode::uucode::types::GraphemeBreakNoControl::Prepend;
                }
                false
            }
        } else {
            false
        };

        let formatted_cps: String = if positions_differ || position_y_differs || cluster_differs || extra_debugging
        {
            let mut writer = String::new();
            let codepoints = &state.codepoints;
            let mut last_cluster: Option<u32> = None;
            for (i, cp) in codepoints.iter().enumerate() {
                if (cp.cluster as i32 >= cell_offset.cluster as i32 - 1 && cp.cluster <= cluster + 1)
                    && cp.codepoint != 0
                // Skip surrogate pair padding
                {
                    if let Some(last) = last_cluster {
                        if cp.cluster != last {
                            writer.push(' ');
                        }
                    }
                    if i == index {
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
                if (cp.cluster as i32 >= cell_offset.cluster as i32 - 1 && cp.cluster <= cluster + 1)
                    && cp.codepoint != 0
                // Skip surrogate pair padding
                {
                    writer.push(char::from_u32(cp.codepoint).unwrap_or('\u{FFFD}'));
                }
            }
            writer
        } else {
            String::new()
        };

        if extra_debugging {
            log::warn!(
                target: LOG,
                "extra debugging of positions index={} cell_offset.cluster={} cluster={} run_offset.cluster={} diff={} pos=({:.2},{:.2}) run_offset=({:.2},{:.2}) cell_offset=({:.2},{:.2}) is_prev_prepend={} cps = {}",
                index,
                cell_offset.cluster,
                cluster,
                run_offset.cluster,
                cluster as isize - cell_offset.cluster as isize,
                x_offset,
                position.y,
                run_offset.x,
                run_offset_y,
                cell_offset.x,
                cell_offset_y,
                is_previous_codepoint_prepend,
                formatted_cps,
            );
        }

        if positions_differ {
            log::warn!(
                target: LOG,
                "position differs from advance: cluster={} pos=({:.2},{:.2}) adv=({:.2},{:.2}) diff=({:.2},{:.2}) cps = {}",
                cluster,
                x_offset,
                position.y,
                advance_x_offset,
                advance_y_offset,
                x_offset_diff,
                y_offset_diff,
                formatted_cps,
            );
        }

        if position_y_differs {
            log::warn!(
                target: LOG,
                "position.y differs from old offset.y: cluster={} pos=({:.2},{:.2}) run_offset=({:.2},{:.2}) cell_offset=({:.2},{:.2}) old offset.y={:.2} cps = {}",
                cluster,
                x_offset,
                position.y,
                run_offset.x,
                run_offset_y,
                cell_offset.x,
                cell_offset_y,
                old_offset_y,
                formatted_cps,
            );
        }

        if cluster_differs {
            log::warn!(
                target: LOG,
                "cell_offset.cluster differs from cluster (potential ligature detected) cell_offset.cluster={} cluster={} run_offset.cluster={} diff={} pos=({:.2},{:.2}) run_offset=({:.2},{:.2}) cell_offset=({:.2},{:.2}) is_prev_prepend={} cps = {}",
                cell_offset.cluster,
                cluster,
                run_offset.cluster,
                cluster as isize - cell_offset.cluster as isize,
                x_offset,
                position.y,
                run_offset.x,
                run_offset_y,
                cell_offset.x,
                cell_offset_y,
                is_previous_codepoint_prepend,
                formatted_cps,
            );
        }
    }
}

impl Drop for Shaper {
    /// Zig `deinit`.
    fn drop(&mut self) {
        self.features.release();
        self.features_no_default.release();
        self.typesetter_attr_dict.release();

        {
            for f in self.cached_fonts.drain(..).flatten() {
                f.release();
            }
        }

        if !self.cf_release_pool.is_empty() {
            for r in self.cf_release_pool.drain(..) {
                // SAFETY: each pooled reference is owned by the pool.
                unsafe { macos::foundation::cf_release(r.0 as *mut c_void) };
            }

            // For tests this logic is normal because we don't want to
            // wait for a release thread. But in production this is a bug
            // and we should warn.
            if !cfg!(test) {
                log::warn!(target: LOG, "BUG: CFRelease pool was not empty, releasing remaining objects");
            }
        }

        // Stop the CF release thread
        {
            if let Err(err) = self.cf_release_thread.stop.notify() {
                log::error!(
                    target: LOG,
                    "error notifying cf release thread to stop, may stall err={err}"
                );
            }
            if let Some(thr) = self.cf_release_thr.take() {
                let _ = thr.join();
            }
        }
        self.cf_release_thread.deinit();
    }
}

/// The hooks for RunIterator.
pub struct RunIteratorHook<'a> {
    pub shaper: &'a mut Shaper,
}

impl run::RunIteratorHook for RunIteratorHook<'_> {
    fn prepare(&mut self) {
        self.shaper.run_state.reset();
        // log.warn("----------- run reset -------------", .{});
    }

    fn add_codepoint(&mut self, cp: u32, cluster: u32) {
        let state = &mut self.shaper.run_state;

        // Build our UTF-16 string for CoreText
        let pair = cp > 0xFFFF;
        if pair {
            let mut units = [0u16; 2];
            let _ = macos::foundation::string_get_surrogate_pair_for_long_character(cp, &mut units);
            state.unichars.extend_from_slice(&units);
        } else {
            state.unichars.push(cp as u16);
        }

        // Build our reverse lookup table for codepoints to clusters
        state.codepoints.push(Codepoint { codepoint: cp, cluster });
        // log.warn("run cp={X}", .{cp});

        // If the UTF-16 codepoint is a pair then we need to insert
        // a dummy entry so that the CTRunGetStringIndices() function
        // maps correctly.
        if pair {
            state.codepoints.push(Codepoint { codepoint: 0, cluster });
        }
    }

    fn finalize(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::font::codepoint_resolver::CodepointResolver;
    use crate::font::collection::{AddOptions, Collection, LoadOptions, SizeAdjustment};
    use crate::font::deferred_face::DeferredFace;
    use crate::font::embedded;
    use crate::font::face::{DesiredSize, Face};
    use crate::font::library::Library;
    use crate::terminal::render::Cells;
    use crate::font::{backend::Backend, Style, OPTIONS};

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
        CodeNewRoman,
        GeistMono,
        Inconsolata,
        JetbrainsMono,
        MonaspaceNeon,
        NerdFont,
    }

    fn add_opts() -> AddOptions {
        AddOptions {
            style: Style::Regular,
            fallback: false,
            size_adjustment: SizeAdjustment::None,
        }
    }

    /// Helper to return a fully initialized shaper.
    #[allow(dead_code)]
    fn test_shaper() -> TestShaper {
        test_shaper_with_font(TestFont::Inconsolata)
    }

    fn test_shaper_with_font(font_req: TestFont) -> TestShaper {
        let test_emoji = embedded::EMOJI;
        let test_emoji_text = embedded::EMOJI_TEXT;
        let test_font = match font_req {
            TestFont::CodeNewRoman => embedded::CODE_NEW_ROMAN,
            TestFont::Inconsolata => embedded::INCONSOLATA,
            TestFont::GeistMono => embedded::GEIST_MONO,
            TestFont::JetbrainsMono => embedded::JETBRAINS_MONO,
            TestFont::MonaspaceNeon => embedded::MONASPACE_NEON,
            TestFont::NerdFont => embedded::TEST_NERD_FONT,
        };

        let lib = Library::init().unwrap();

        let mut c = Collection::init();
        c.load_options = Some(LoadOptions::new(lib.clone()));

        // Setup group
        c.add(Face::init(lib.clone(), test_font, DesiredSize::new(12.0).into()).unwrap(), add_opts())
            .unwrap();

        if OPTIONS.backend != Backend::Coretext {
            // Coretext doesn't support Noto's format
            c.add(Face::init(lib.clone(), test_emoji, DesiredSize::new(12.0).into()).unwrap(), add_opts())
                .unwrap();
        } else {
            // On CoreText we want to load Apple Emoji, we should have it.
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

    /// Return a fully initialized shaper by discovering a named font on the system.
    /// Returns `None` where the Zig version returns an error.
    #[allow(dead_code)]
    fn test_shaper_with_discovered_font(font_req: &str) -> Option<TestShaper> {
        let lib = LibGuard(Library::init().unwrap());

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
            c.add(face.load(lib.0.clone(), DesiredSize::new(12.0).into()).ok()?, add_opts())
                .unwrap();
        }

        let grid = Box::new(SharedGrid::init(CodepointResolver::new(c)).unwrap());

        let shaper = Shaper::init(shape::Options::default()).unwrap();

        Some(TestShaper { shaper, grid, lib })
    }

    // -----------------------------------------------------------------
    // Ported Zig tests. They use terminal::Terminal, its vtStream and
    // terminal::RenderState.
    // -----------------------------------------------------------------

    fn new_terminal(cols: u16, rows: u16) -> crate::terminal::Terminal {
        crate::terminal::Terminal::init(crate::terminal::terminal::Options {
            cols,
            rows,
            ..Default::default()
        })
        .unwrap()
    }

    fn row_cells(state: &crate::terminal::RenderState, row: usize) -> Cells {
        state.row_data.cells[row].clone()
    }

    fn update_row(t: &mut crate::terminal::Terminal, row: usize) -> Cells {
        let mut state = crate::terminal::RenderState::empty();
        state.update(t);
        row_cells(&state, row)
    }

    /// Build a terminal, feed `inputs` through its vt stream (optionally
    /// setting grapheme clustering first) and return the cells of `row`.
    fn terminal_row(cols: u16, rows: u16, grapheme_cluster: Option<bool>, inputs: &[&[u8]], row: usize) -> Cells {
        let mut t = new_terminal(cols, rows);
        if let Some(v) = grapheme_cluster {
            t.modes.set(crate::terminal::modes::Mode::GraphemeCluster, v);
        }
        {
            let mut s = t.vt_stream();
            for input in inputs {
                s.next_slice(input);
            }
        }
        update_row(&mut t, row)
    }

    /// Count the runs of `cells`, shaping each, checking every run with
    /// `check` (Zig: the `while (try it.next(alloc)) |run|` loops).
    fn for_each_run(
        testdata: &mut TestShaper,
        cells: &Cells,
        opts_fn: impl FnOnce(RunOptions<'_>) -> RunOptions<'_>,
        mut check: impl FnMut(&TextRun<'_>, &[Cell]),
    ) -> usize {
        let grid: &SharedGrid = &testdata.grid;
        let mut it = testdata.shaper.run_iterator(opts_fn(RunOptions { cells: cells.slice(), ..RunOptions::new(grid) }));
        let mut count = 0;
        while let Some(run) = it.next().unwrap() {
            count += 1;
            let shaped = it.hooks.shaper.shape(run).unwrap();
            check(&run, shaped);
        }
        count
    }

    fn count_runs_no_shape(testdata: &mut TestShaper, cells: &Cells) -> usize {
        let grid: &SharedGrid = &testdata.grid;
        let mut it = testdata.shaper.run_iterator(RunOptions { cells: cells.slice(), ..RunOptions::new(grid) });
        let mut count = 0;
        while it.next().unwrap().is_some() {
            count += 1;
        }
        count
    }

    #[test]
    fn run_iterator() {
        let mut testdata = test_shaper();

        assert_eq!(1, count_runs_no_shape(&mut testdata, &terminal_row(5, 3, None, &[b"ABCD"], 0)));

        // Spaces should be part of a run
        assert_eq!(1, count_runs_no_shape(&mut testdata, &terminal_row(10, 3, None, &[b"ABCD   EFG"], 0)));

        assert_eq!(3, count_runs_no_shape(&mut testdata, &terminal_row(5, 3, None, &["A😃D".as_bytes()], 0)));

        // Bad ligatures
        for bad in [b"fl".as_slice(), b"fi", b"st"] {
            assert_eq!(2, count_runs_no_shape(&mut testdata, &terminal_row(5, 3, None, &[bad], 0)));
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
                // Set red background
                s.next_slice(b"\x1b[48;2;255;0;0m");
                s.next_slice(b"A");
            }

            // Get our first row
            for x in [1, 2] {
                let list_cell = t
                    .screens
                    .active
                    .pages
                    .get_cell(crate::terminal::point::Point::Active(crate::terminal::point::Coordinate { x, y: 0 }))
                    .unwrap();
                let mut cell = crate::terminal::page::Cell::default();
                cell.set_content_tag(crate::terminal::page::ContentTag::BgColorRgb);
                cell.set_content_color_rgb(crate::terminal::color::Rgb { r: 0xFF, g: 0, b: 0 });
                // SAFETY: the list cell points at a live cell.
                unsafe { *list_cell.cell = cell };
            }

            let cells = update_row(&mut t, 0);

            // Get our run iterator
            let grid: &SharedGrid = &testdata.grid;
            let mut it = testdata.shaper.run_iterator(RunOptions { cells: cells.slice(), ..RunOptions::new(grid) });
            {
                let run = it.next().unwrap().unwrap();
                let cells = it.hooks.shaper.shape(run).unwrap();
                assert_eq!(3, cells.len());
            }
            assert!(it.next().unwrap().is_none());
        }
    }

    #[test]
    fn shape() {
        let mut testdata = test_shaper();

        let buf: String = [0x1F44D, 0x1F44D, 0x1F3FD] // Thumbs up plain x2, Medium skin tone
            .iter()
            .map(|&c| char::from_u32(c).unwrap())
            .collect();

        let cells = terminal_row(10, 3, None, &[buf.as_bytes()], 0);
        assert_eq!(1, for_each_run(&mut testdata, &cells, |o| o, |_, _| {}));
    }

    #[test]
    fn shape_nerd_fonts() {
        let mut testdata = test_shaper_with_font(TestFont::NerdFont);

        let buf: String = [' ' as u32, 0xF024B, ' ' as u32] // space, nf-md-folder, space
            .iter()
            .map(|&c| char::from_u32(c).unwrap())
            .collect();

        let cells = terminal_row(10, 3, None, &[buf.as_bytes()], 0);
        assert_eq!(1, for_each_run(&mut testdata, &cells, |o| o, |_, _| {}));
    }

    /// Every run has `run_cells` cells and shapes into `glyphs` glyphs;
    /// there is exactly one run.
    fn expect_single_lig(testdata: &mut TestShaper, input: &[u8], run_cells: u16, glyphs: usize) {
        let cells = terminal_row(5, 3, None, &[input], 0);
        let count = for_each_run(testdata, &cells, |o| o, |run, cells| {
            assert_eq!(run_cells, run.cells);
            assert_eq!(glyphs, cells.len());
        });
        assert_eq!(1, count);
    }

    #[test]
    fn shape_inconsolata_ligs() {
        let mut testdata = test_shaper();
        expect_single_lig(&mut testdata, b">=", 2, 1);
        expect_single_lig(&mut testdata, b"===", 3, 1);
    }

    #[test]
    fn shape_monaspace_ligs() {
        let mut testdata = test_shaper_with_font(TestFont::MonaspaceNeon);
        expect_single_lig(&mut testdata, b"===", 3, 1);
    }

    // https://github.com/mitchellh/ghostty/issues/1708
    #[test]
    fn shape_left_replaced_lig_in_last_run() {
        let mut testdata = test_shaper_with_font(TestFont::GeistMono);
        expect_single_lig(&mut testdata, b"!==", 3, 1);
    }

    // https://github.com/mitchellh/ghostty/issues/1708
    #[test]
    fn shape_left_replaced_lig_in_early_run() {
        let mut testdata = test_shaper_with_font(TestFont::GeistMono);

        let cells = terminal_row(5, 3, None, &[b"!==X"], 0);

        let grid: &SharedGrid = &testdata.grid;
        let mut it = testdata.shaper.run_iterator(RunOptions { cells: cells.slice(), ..RunOptions::new(grid) });

        let run = it.next().unwrap().unwrap();

        assert_eq!(4, run.cells);

        let cells = it.hooks.shaper.shape(run).unwrap();
        assert_eq!(2, cells.len());
    }

    // https://github.com/mitchellh/ghostty/issues/1664
    #[test]
    fn shape_u_3c9_with_jb_mono() {
        let mut testdata = test_shaper_with_font(TestFont::JetbrainsMono);

        let cells = terminal_row(10, 3, None, &["\u{03C9} foo".as_bytes()], 0);

        let mut cell_count: usize = 0;
        let run_count = for_each_run(&mut testdata, &cells, |o| o, |_, cells| cell_count += cells.len());
        assert_eq!(1, run_count);
        assert_eq!(5, cell_count);
    }

    #[test]
    fn shape_emoji_width() {
        let mut testdata = test_shaper();

        let cells = terminal_row(5, 3, None, &["👍".as_bytes()], 0);
        let count = for_each_run(&mut testdata, &cells, |o| o, |_, cells| assert_eq!(1, cells.len()));
        assert_eq!(1, count);
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
            *cell = crate::terminal::page::Cell::init(0x1F9D4); // Person with beard
            let graphemes: [u32; 4] = [
                0x1F3FB, // Light skin tone (Fitz 1-2)
                0x200D,  // ZWJ
                0x2642,  // Male sign
                0xFE0F,  // Emoji presentation selector
            ];
            page.set_graphemes(row, cell, &graphemes).unwrap();
        }

        let cells = update_row(&mut t, 1);

        let count = for_each_run(&mut testdata, &cells, |o| o, |_, cells| assert_eq!(1, cells.len()));
        assert_eq!(1, count);
    }

    fn expect_variation_selector(selector: u32) {
        let mut testdata = test_shaper();

        let mut buf = String::new();
        buf.push(char::from_u32(0x270C).unwrap()); // Victory sign (default text)
        buf.push(char::from_u32(selector).unwrap()); // ZWJ to force text/color

        let cells = terminal_row(10, 3, None, &[buf.as_bytes()], 0);
        let count = for_each_run(&mut testdata, &cells, |o| o, |_, cells| assert_eq!(1, cells.len()));
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

        // 5 spaces forward
        let cells = terminal_row(30, 3, None, &[b"A", b"\x1b[5C", b"B"], 0);

        let mut count: usize = 0;
        for_each_run(&mut testdata, &cells, |o| o, |_, cells| {
            count += 1;
            assert_eq!(1, count);
            assert_eq!(7, cells.len());
        });
    }

    #[test]
    fn shape_combining_characters() {
        let mut testdata = test_shaper();

        let buf: String = ['n', '\u{0308}', '\u{0308}', 'a'].iter().collect();

        // Enable grapheme clustering
        let cells = terminal_row(30, 3, Some(true), &[buf.as_bytes()], 0);

        let count = for_each_run(&mut testdata, &cells, |o| o, |_, cells| {
            assert_eq!(4, cells.len());
            assert_eq!(0, cells[0].x);
            assert_eq!(0, cells[1].x);
            assert_eq!(0, cells[2].x);
            assert_eq!(1, cells[3].x);
        });
        assert_eq!(1, count);
    }

    // This test exists because the string it uses causes CoreText to output a
    // non-monotonic run, which we need to handle by sorting the resulting buffer.
    #[test]
    fn shape_devanagari_string() {
        // We need a font that supports devanagari for this to work, if we can't
        // find Arial Unicode MS, which is a system font on macOS, we just skip
        // the test.
        let Some(mut testdata) = test_shaper_with_discovered_font("Arial Unicode MS") else {
            return;
        };

        // Disable grapheme clustering
        let cells = terminal_row(30, 3, Some(false), &["अपार्टमेंट".as_bytes()], 0);

        let grid: &SharedGrid = &testdata.grid;
        let mut it = testdata.shaper.run_iterator(RunOptions { cells: cells.slice(), ..RunOptions::new(grid) });

        let run = it.next().unwrap();
        assert!(run.is_some());
        {
            let cells = it.hooks.shaper.shape(run.unwrap()).unwrap();

            // To understand the `x`/`cluster` assertions here, run with the "For
            // debugging positions" code turned on and `extra_debugging` set to true.
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

    #[test]
    fn shape_tai_tham_vowels_position_differs_from_advance() {
        // We need a font that supports Tai Tham for this to work, if we can't find
        // Noto Sans Tai Tham, which is a system font on macOS, we just skip the
        // test.
        let Some(mut testdata) = test_shaper_with_discovered_font("Noto Sans Tai Tham") else {
            return;
        };

        let buf: String = ['\u{1a2F}', '\u{1a70}'].iter().collect(); // ᨯ ᩰ

        // Enable grapheme clustering
        let cells = terminal_row(30, 3, Some(true), &[buf.as_bytes()], 0);

        let cell_width = testdata.grid.metrics.cell_width;
        let count = for_each_run(&mut testdata, &cells, |o| o, |_, cells| {
            assert_eq!(2, cells.len());
            assert_eq!(0, cells[0].x);
            assert_eq!(0, cells[1].x);

            // The first glyph renders in the next cell
            assert_eq!(cell_width as i16, cells[0].x_offset);
            assert_eq!(0, cells[1].x_offset);
        });
        assert_eq!(1, count);
    }

    #[test]
    fn shape_tai_tham_letters_position_y_differs_from_advance() {
        // We need a font that supports Tai Tham for this to work, if we can't find
        // Noto Sans Tai Tham, which is a system font on macOS, we just skip the
        // test.
        let Some(mut testdata) = test_shaper_with_discovered_font("Noto Sans Tai Tham") else {
            return;
        };

        let buf: String = [
            // First grapheme cluster:
            '\u{1a49}', // HA
            '\u{1a60}', // SAKOT
            // Second grapheme cluster, combining with the first in a ligature:
            '\u{1a3f}', // YA
            '\u{1a69}', // U
        ]
        .iter()
        .collect();

        // Enable grapheme clustering
        let cells = terminal_row(30, 3, Some(true), &[buf.as_bytes()], 0);

        let count = for_each_run(&mut testdata, &cells, |o| o, |_, cells| {
            assert_eq!(3, cells.len());
            assert_eq!(0, cells[0].x);
            assert_eq!(0, cells[1].x);
            assert_eq!(0, cells[2].x); // U from second grapheme

            // The U glyph renders at a y below zero
            assert_eq!(-3, cells[2].y_offset);
        });
        assert_eq!(1, count);
    }

    #[test]
    fn shape_javanese_ligatures() {
        // We need a font that supports Javanese for this to work, if we can't find
        // Noto Sans Javanese Regular, which is a system font on macOS, we just
        // skip the test.
        let Some(mut testdata) = test_shaper_with_discovered_font("Noto Sans Javanese") else {
            return;
        };

        let buf: String = [
            // First grapheme cluster:
            '\u{a9a4}', // NA
            '\u{a9c0}', // PANGKON
            // Second grapheme cluster, combining with the first in a ligature:
            '\u{a9b2}', // HA
            '\u{a9b8}', // Vowel sign SUKU
        ]
        .iter()
        .collect();

        // Enable grapheme clustering
        let cells = terminal_row(30, 3, Some(true), &[buf.as_bytes()], 0);

        let cell_width = testdata.grid.metrics.cell_width;
        let count = for_each_run(&mut testdata, &cells, |o| o, |_, cells| {
            assert_eq!(3, cells.len());
            assert_eq!(0, cells[0].x);
            assert_eq!(0, cells[1].x);
            assert_eq!(0, cells[2].x);

            // The vowel sign SUKU renders with correct x_offset
            assert!(cells[2].x_offset as i64 > 3 * cell_width as i64);
        });
        assert_eq!(1, count);
    }

    #[test]
    fn shape_chakma_vowel_sign_with_ligature_vowel_sign_renders_first() {
        // We need a font that supports Chakma for this to work, if we can't find
        // Noto Sans Chakma Regular, which is a system font on macOS, we just skip
        // the test.
        let Some(mut testdata) = test_shaper_with_discovered_font("Noto Sans Chakma") else {
            return;
        };

        let buf: String = [
            // First grapheme cluster:
            '\u{1111d}', // BAA
            // Second grapheme cluster:
            '\u{11116}', // TAA
            '\u{11133}', // Virama
            // Third grapheme cluster, combining with the second in a ligature:
            '\u{11120}', // YYAA
            '\u{1112c}', // Vowel Sign U
        ]
        .iter()
        .collect();

        // Enable grapheme clustering
        let cells = terminal_row(30, 3, Some(true), &[buf.as_bytes()], 0);

        let count = for_each_run(&mut testdata, &cells, |o| o, |_, cells| {
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
        });
        assert_eq!(1, count);
    }

    #[test]
    fn shape_bengali_ligatures_with_out_of_order_vowels() {
        // We need a font that supports Bengali for this to work, if we can't find
        // Arial Unicode MS, which is a system font on macOS, we just skip the
        // test.
        let Some(mut testdata) = test_shaper_with_discovered_font("Arial Unicode MS") else {
            return;
        };

        let buf: String = [
            // First grapheme cluster:
            '\u{09b0}', // RA
            '\u{09be}', // Vowel sign AA
            // Second grapheme cluster:
            '\u{09b7}', // SSA
            '\u{09cd}', // Virama
            // Third grapheme cluster, combining with the second in a ligature:
            '\u{099f}', // TTA
            '\u{09cd}', // Virama
            // Fourth grapheme cluster, combining with the previous two in a ligature:
            '\u{09b0}', // RA
            '\u{09c7}', // Vowel sign E
        ]
        .iter()
        .collect();

        // Enable grapheme clustering
        let cells = terminal_row(30, 3, Some(true), &[buf.as_bytes()], 0);

        let count = for_each_run(&mut testdata, &cells, |o| o, |_, cells| {
            assert_eq!(8, cells.len());
            assert_eq!(0, cells[0].x);
            assert_eq!(0, cells[1].x);
            // See the giant "We need to reset the `cell_offset`" comment, but here
            // we should technically have the rest of these be `x` of 2, but that
            // would require going back in the stream to adjust past cells, and
            // we don't take on that complexity.
            assert_eq!(0, cells[2].x);
            assert_eq!(0, cells[3].x);
            assert_eq!(0, cells[4].x);
            assert_eq!(0, cells[5].x);
            assert_eq!(0, cells[6].x);
            assert_eq!(0, cells[7].x);

            // The vowel sign E renders before the SSA:
            assert!(cells[2].x_offset < cells[3].x_offset);
        });
        assert_eq!(1, count);
    }

    #[test]
    fn shape_box_glyphs() {
        let mut testdata = test_shaper();

        let buf: String = ['\u{2500}', '\u{2501}'].iter().collect(); // horiz line

        let cells = terminal_row(10, 3, None, &[buf.as_bytes()], 0);

        let count = for_each_run(&mut testdata, &cells, |o| o, |_, cells| {
            assert_eq!(2, cells.len());
            assert_eq!(0x2500, cells[0].glyph_index);
            assert_eq!(0, cells[0].x);
            assert_eq!(0x2501, cells[1].glyph_index);
            assert_eq!(1, cells[1].x);
        });
        assert_eq!(1, count);
    }

    #[test]
    fn shape_selection_boundary() {
        let mut testdata = test_shaper();

        let cols: u16 = 10;
        let cells = terminal_row(cols, 3, None, &[b"a1b2c3d4e5"], 0);

        // Full line selection
        assert_eq!(1, for_each_run(&mut testdata, &cells, |o| RunOptions { selection: Some([0, cols - 1]), ..o }, |_, _| {}));

        // Offset x, goes to end of line selection
        assert_eq!(2, for_each_run(&mut testdata, &cells, |o| RunOptions { selection: Some([2, cols - 1]), ..o }, |_, _| {}));

        // Offset x, starts at beginning of line
        assert_eq!(2, for_each_run(&mut testdata, &cells, |o| RunOptions { selection: Some([0, 3]), ..o }, |_, _| {}));

        // Selection only subset of line
        assert_eq!(3, for_each_run(&mut testdata, &cells, |o| RunOptions { selection: Some([1, 3]), ..o }, |_, _| {}));

        // Selection only one character
        assert_eq!(3, for_each_run(&mut testdata, &cells, |o| RunOptions { selection: Some([1, 1]), ..o }, |_, _| {}));
    }

    #[test]
    fn shape_cursor_boundary() {
        let mut testdata = test_shaper();

        let cells = terminal_row(10, 3, None, &[b"a1b2c3d4e5"], 0);

        // No cursor is full line
        assert_eq!(1, for_each_run(&mut testdata, &cells, |o| o, |_, _| {}));

        {
            // Cursor at index 0 is two runs
            assert_eq!(2, for_each_run(&mut testdata, &cells, |o| RunOptions { cursor_x: Some(0), ..o }, |_, _| {}));
            // And without cursor splitting remains one
            assert_eq!(1, for_each_run(&mut testdata, &cells, |o| o, |_, _| {}));
        }

        {
            // Cursor at index 1 is three runs
            assert_eq!(3, for_each_run(&mut testdata, &cells, |o| RunOptions { cursor_x: Some(1), ..o }, |_, _| {}));
            // And without cursor splitting remains one
            assert_eq!(1, for_each_run(&mut testdata, &cells, |o| o, |_, _| {}));
        }
        {
            // Cursor at last col is two runs
            assert_eq!(2, for_each_run(&mut testdata, &cells, |o| RunOptions { cursor_x: Some(9), ..o }, |_, _| {}));
            // And without cursor splitting remains one
            assert_eq!(1, for_each_run(&mut testdata, &cells, |o| o, |_, _| {}));
        }
    }

    #[test]
    fn shape_cursor_boundary_and_colored_emoji() {
        let mut testdata = test_shaper();

        let cells = terminal_row(3, 10, None, &["👍🏼".as_bytes()], 0);

        // No cursor is full line
        assert_eq!(1, for_each_run(&mut testdata, &cells, |o| o, |_, _| {}));

        // Cursor on emoji does not split it
        assert_eq!(1, for_each_run(&mut testdata, &cells, |o| RunOptions { cursor_x: Some(0), ..o }, |_, _| {}));
        assert_eq!(1, for_each_run(&mut testdata, &cells, |o| o, |_, _| {}));
        assert_eq!(1, for_each_run(&mut testdata, &cells, |o| RunOptions { cursor_x: Some(1), ..o }, |_, _| {}));
        assert_eq!(1, for_each_run(&mut testdata, &cells, |o| o, |_, _| {}));
    }

    #[test]
    fn shape_cell_attribute_change() {
        let mut testdata = test_shaper();

        // Plain >= should shape into 1 run
        {
            let cells = terminal_row(10, 3, None, &[b">="], 0);
            assert_eq!(1, for_each_run(&mut testdata, &cells, |o| o, |_, _| {}));
        }

        // Bold vs regular should split
        {
            let cells = terminal_row(3, 10, None, &[b">", b"\x1b[1m", b"="], 0);
            assert_eq!(2, for_each_run(&mut testdata, &cells, |o| o, |_, _| {}));
        }

        // Changing fg color should split
        {
            let cells = terminal_row(
                3,
                10,
                None,
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
            assert_eq!(2, for_each_run(&mut testdata, &cells, |o| o, |_, _| {}));
        }

        // Changing bg color should NOT split
        {
            let cells = terminal_row(
                3,
                10,
                None,
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
            assert_eq!(1, for_each_run(&mut testdata, &cells, |o| o, |_, _| {}));
        }

        // Same bg color should not split
        {
            let cells = terminal_row(
                3,
                10,
                None,
                &[
                    // RGB 1, 2, 3 bg
                    b"\x1b[48;2;1;2;3m",
                    b">",
                    b"=",
                ],
                0,
            );
            assert_eq!(1, for_each_run(&mut testdata, &cells, |o| o, |_, _| {}));
        }
    }

    #[test]
    fn shape_high_plane_sprite_font_codepoint() {
        // While creating runs, the CoreText shaper uses `0` codepoints to
        // pad its codepoint list to account for high plane characters which
        // use two UTF-16 code units. This is so that, after shaping, the string
        // indices can be used to find the originating codepoint / cluster.
        //
        // This is a problem for special (sprite) fonts, which need to be "shaped"
        // by simply returning the input codepoints verbatim. We include logic to
        // skip `0` codepoints when constructing the shaped run for sprite fonts,
        // this test verifies that it works correctly.

        let mut testdata = test_shaper();

        // U+1FB70: Vertical One Eighth Block-2
        let cells = terminal_row(10, 3, None, &["\u{1FB70}".as_bytes()], 0);

        let grid: &SharedGrid = &testdata.grid;
        let mut it = testdata.shaper.run_iterator(RunOptions { cells: cells.slice(), ..RunOptions::new(grid) });
        // We should get one run
        let run = it.next().unwrap().unwrap();
        // The run state should have the UTF-16 encoding of the character.
        assert_eq!(&[0xD83Eu16, 0xDF70][..], &it.hooks.shaper.run_state.unichars[..]);
        // The codepoint list should be padded.
        assert_eq!(
            &[
                Codepoint { codepoint: 0x1FB70, cluster: 0 },
                Codepoint { codepoint: 0, cluster: 0 },
            ][..],
            &it.hooks.shaper.run_state.codepoints[..],
        );
        // And when shape it
        let cells = it.hooks.shaper.shape(run).unwrap().to_vec();
        // we should have
        // - 1 cell
        assert_eq!(1, run.cells);
        // - at position 0
        assert_eq!(0, run.offset);
        // - with 1 glyph in it
        assert_eq!(1, cells.len());
        // - at position 0
        assert_eq!(0, cells[0].x);
        // - the glyph index should be equal to the codepoint
        assert_eq!(0x1FB70, cells[0].glyph_index);
        // - it should be a sprite font
        assert!(run.font_index.special().is_some());
        // And we should get a null run after that
        assert!(it.next().unwrap().is_none());
    }
}
