//! Port of `src/font/Collection.zig`.
//!
//! STATUS: ported
//!
//! A font collection is a list of faces of different styles. The list is
//! ordered by priority (per style). All fonts in a collection share the same
//! size so they can be used interchangeably in cases a glyph is missing in one
//! and present in another.
//!
//! The purpose of a collection is to store a list of fonts by style
//! and priority order. A collection does not handle searching for font
//! callbacks, rasterization, etc. For this, see CodepointResolver.
//!
//! The collection can contain both loaded and deferred faces. Deferred faces
//! typically use less memory while still providing some necessary information
//! such as codepoint support, presentation, etc. This is useful for looking
//! for fallback fonts as efficiently as possible. For example, when the glyph
//! "X" is not found, we can quickly search through deferred fonts rather
//! than loading the font completely.

use std::ptr::NonNull;

use crate::config::FontSyntheticStyle;
use crate::datastruct::segmented_list::SegmentedList;
use crate::font::deferred_face::{self, DeferredFace};
use crate::font::face::{self, DesiredSize, Face, FreetypeLoadFlags};
use crate::font::library::Library;
use crate::font::metrics::{self, FaceMetrics, Metrics};
use crate::font::{Presentation, Style};

const LOG: &str = "font_collection";

/// The error type of loading a face (`Face.init`, `Face.setSize`, ...).
pub type FaceError = deferred_face::FaceError;

pub struct Collection {
    /// The available faces we have. This shouldn't be modified manually.
    /// Instead, use the functions available on Collection.
    pub faces: StyleArray,

    /// The metric modifiers to use for this collection. The memory
    /// for this is owned by the user and is not freed by the collection.
    ///
    /// Call `Collection.updateMetrics` to recompute the
    /// collection's metrics after making changes to these.
    pub metric_modifiers: metrics::ModifierSet,

    /// Metrics for this collection. Call `Collection.updateMetrics` to (re)compute
    /// these after adding a primary font or making changes to `metric_modifiers`.
    pub metrics: Option<Metrics>,

    /// The face metrics for the primary face in the collection.
    ///
    /// We keep this around so we don't need to re-compute it when calculating
    /// the scale factor for additional fonts added to the collection.
    pub primary_face_metrics: Option<FaceMetrics>,

    /// The load options for deferred faces in the face list. If this
    /// is not set, then deferred faces will not be loaded. Attempting to
    /// add a deferred face will result in an error.
    pub load_options: Option<LoadOptions>,
}

// SAFETY: the only non-thread-safe members are the alias pointers (which
// point into this collection's own pointer-stable segmented lists) and the
// face handles. Like the Zig code, all mutation goes through `&mut self`
// (callers such as `SharedGrid` serialize access with a lock).
unsafe impl Send for Collection {}
unsafe impl Sync for Collection {}

impl Default for Collection {
    fn default() -> Collection {
        Collection::init()
    }
}

impl std::fmt::Debug for Collection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Collection")
            .field("regular", &self.faces.get(Style::Regular).count())
            .field("bold", &self.faces.get(Style::Bold).count())
            .field("italic", &self.faces.get(Style::Italic).count())
            .field("bold_italic", &self.faces.get(Style::BoldItalic).count())
            .field("metrics", &self.metrics)
            .finish_non_exhaustive()
    }
}

/// Options for adding a face to the collection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddOptions {
    /// What style this face is.
    pub style: Style,
    /// What size adjustment to use.
    pub size_adjustment: SizeAdjustment,
    /// Whether this is a fallback face.
    pub fallback: bool,
}

macro_rules! error_enum {
    ($(#[$m:meta])* pub enum $name:ident { $($(#[$vm:meta])* $v:ident $(($t:ty))?),* $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum $name { $($(#[$vm])* $v $(($t))?),* }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                std::fmt::Debug::fmt(self, f)
            }
        }

        impl std::error::Error for $name {}
    };
}

error_enum! {
    /// Zig `AddError` (minus `Allocator.Error`).
    pub enum AddError {
        /// There's no more room in the collection.
        CollectionFull,
        /// The call to `face.setSize` failed.
        SetSizeFailed,
    }
}

error_enum! {
    /// Zig `AddDeferredError` (minus `Allocator.Error`).
    pub enum AddDeferredError {
        /// There's no more room in the collection.
        CollectionFull,
        /// `load_options` is null, can't do deferred loading.
        DeferredLoadingUnavailable,
    }
}

error_enum! {
    pub enum EntryError {
        /// Index represents a special font (built-in) and these don't
        /// have an associated face. This should be caught upstream and use
        /// alternate logic.
        SpecialHasNoFace,

        /// Invalid index.
        IndexOutOfBounds,
    }
}

error_enum! {
    /// The (inferred) error set of `getFace`.
    pub enum GetFaceError {
        SpecialHasNoFace,
        IndexOutOfBounds,
        DeferredLoadingUnavailable,
        /// Loading the deferred face failed.
        Load(deferred_face::Error),
        /// Resizing the loaded face failed.
        Face(FaceError),
    }
}

impl From<EntryError> for GetFaceError {
    fn from(e: EntryError) -> GetFaceError {
        match e {
            EntryError::SpecialHasNoFace => GetFaceError::SpecialHasNoFace,
            EntryError::IndexOutOfBounds => GetFaceError::IndexOutOfBounds,
        }
    }
}

error_enum! {
    /// Zig `CompleteError` (minus `Allocator.Error`).
    pub enum CompleteError {
        DefaultUnavailable,
    }
}

error_enum! {
    /// The (inferred) error set of `syntheticBold`/`syntheticItalic`.
    pub enum SyntheticError {
        SyntheticBoldUnavailable,
        SyntheticItalicUnavailable,
        DeferredLoadingUnavailable,
        GetFace(GetFaceError),
        Face(FaceError),
    }
}

error_enum! {
    /// Zig `SetSizeError` (`UpdateMetricsError || ...`).
    pub enum SetSizeError {
        CannotLoadPrimaryFont,
        /// `self.load_options` is `null`.
        DeferredLoadingUnavailable,
        /// The call to `face.setSize` failed.
        SetSizeFailed,
    }
}

impl From<UpdateMetricsError> for SetSizeError {
    fn from(e: UpdateMetricsError) -> SetSizeError {
        match e {
            UpdateMetricsError::CannotLoadPrimaryFont => SetSizeError::CannotLoadPrimaryFont,
        }
    }
}

error_enum! {
    pub enum UpdateMetricsError {
        CannotLoadPrimaryFont,
    }
}

impl Collection {
    /// Initialize an empty collection.
    pub fn init() -> Collection {
        // Initialize our styles array, preallocating some space that is
        // likely to be used.
        Collection {
            faces: StyleArray::init_fill(),
            metric_modifiers: metrics::ModifierSet::default(),
            metrics: None,
            primary_face_metrics: None,
            load_options: None,
        }
    }

    /// Add a face to the collection. This face will be added next in priority if
    /// others exist already, i.e. it'll be the _last_ to be searched for a glyph
    /// in that list.
    ///
    /// If no error is encountered then the collection takes ownership of the face,
    /// in which case face will be deallocated when the collection is deallocated.
    /// (In Rust the face is dropped on error too; the Zig caller frees it.)
    ///
    /// When added, the size of the face will be adjusted to match `load_options`.
    ///
    /// Returns the index for the added face.
    pub fn add(&mut self, face: Face, opts: AddOptions) -> Result<Index, AddError> {
        // We have some special indexes so we must never pass those.
        let idx = self.faces.get(opts.style).count();
        if idx >= Special::START as usize - 1 {
            return Err(AddError::CollectionFull);
        }

        let mut owned_face = face;

        // Scale factor to adjust the size of the added face.
        let scale_factor = self.scale_factor(owned_face.get_metrics(), opts.size_adjustment);

        // If we have load options, we update the size to ensure
        // it's matches and is normalized to the primary if possible.
        if let Some(load_opts) = &self.load_options {
            let mut new_opts = load_opts.clone();
            new_opts.size.points *= scale_factor as f32;
            owned_face
                .set_size(new_opts.face_options())
                .map_err(|_| AddError::SetSizeFailed)?;
        }

        self.faces.get_mut(opts.style).append(EntryOrAlias::Entry(Entry {
            face: AnyFace::Loaded(owned_face),
            fallback: opts.fallback,
            scale_factor: ScaleFactor::Scale(scale_factor),
        }));

        Ok(Index {
            style: opts.style,
            idx: idx as IndexInt,
        })
    }

    /// Add a deferred face to the collection.
    ///
    /// Returns the index for the added face.
    pub fn add_deferred(
        &mut self,
        face: DeferredFace,
        opts: AddOptions,
    ) -> Result<Index, AddDeferredError> {
        if self.load_options.is_none() {
            return Err(AddDeferredError::DeferredLoadingUnavailable);
        }

        // We have some special indexes so we must never pass those.
        let idx = self.faces.get(opts.style).count();
        if idx >= Special::START as usize - 1 {
            return Err(AddDeferredError::CollectionFull);
        }

        self.faces.get_mut(opts.style).append(EntryOrAlias::Entry(Entry {
            face: AnyFace::Deferred(face),
            fallback: opts.fallback,
            scale_factor: ScaleFactor::Adjustment(opts.size_adjustment),
        }));

        Ok(Index {
            style: opts.style,
            idx: idx as IndexInt,
        })
    }

    /// Return the Face represented by a given Index. The returned pointer
    /// is only valid as long as this collection is not modified.
    ///
    /// This will initialize the face if it is deferred and not yet loaded,
    /// which can fail.
    pub fn get_face(&mut self, index: Index) -> Result<&mut Face, GetFaceError> {
        let entry: *mut Entry = self.get_entry(index)?;
        // SAFETY: `entry` points into one of our segmented lists, which are
        // pointer-stable, and nothing removes entries while we hold it.
        unsafe { self.get_face_from_entry(entry) }
    }

    /// Convenience for `getFace(.{ .style = style })` (index 0 of a style).
    pub fn get_face_style(&mut self, style: Style) -> Result<&mut Face, GetFaceError> {
        self.get_face(Index { style, idx: 0 })
    }

    /// Get the unaliased entry from an index
    pub fn get_entry(&mut self, index: Index) -> Result<&mut Entry, EntryError> {
        if index.special().is_some() {
            return Err(EntryError::SpecialHasNoFace);
        }
        let list = self.faces.get_mut(index.style);
        if index.idx as usize >= list.len {
            return Err(EntryError::IndexOutOfBounds);
        }
        Ok(list.at_mut(index.idx as usize).get_entry())
    }

    /// Get the face from an entry.
    ///
    /// This entry must not be an alias.
    ///
    /// # Safety
    /// `entry` must point to a live entry of this collection.
    unsafe fn get_face_from_entry<'a>(&'a mut self, entry: *mut Entry) -> Result<&'a mut Face, GetFaceError> {
        if let AnyFace::Deferred(d) = &mut (*entry).face {
            let Some(mut opts) = self.load_options.clone() else {
                return Err(GetFaceError::DeferredLoadingUnavailable);
            };

            // Load the face.
            let mut face = d.load(opts.library.clone(), opts.face_options()).map_err(GetFaceError::Load)?;

            // Calculate the scale factor for this
            // entry now that we have a loaded face.
            if let ScaleFactor::Adjustment(adjustment) = (*entry).scale_factor {
                let factor = self.scale_factor(face.get_metrics(), adjustment);
                (*entry).scale_factor = ScaleFactor::Scale(factor);
            }

            // If our scale factor is something other
            // than 1.0 then we need to resize the face.
            let scale = (*entry).scale_factor.scale();
            if scale != 1.0 {
                opts.size.points *= scale as f32;
                face.set_size(opts.face_options())
                    .map_err(|e| GetFaceError::Face(FaceError::from(e)))?;
            }

            // Set the loaded face on the entry. This drops (deinits) the
            // deferred face now that we have loaded it and are past any
            // possible errors.
            (*entry).face = AnyFace::Loaded(face);
        }

        match &mut (*entry).face {
            AnyFace::Loaded(f) => Ok(f),
            AnyFace::Deferred(_) => unreachable!(),
        }
    }

    /// Return the index of the font in this collection that contains
    /// the given codepoint, style, and presentation. If no font is found,
    /// null is returned.
    ///
    /// This does not trigger font loading; deferred fonts can be
    /// searched for codepoints.
    pub fn get_index(&self, cp: u32, style: Style, p_mode: PresentationMode) -> Option<Index> {
        let list = self.faces.get(style);
        let mut it = list.const_iterator(0);
        let mut i: usize = 0;
        while let Some(entry_or_alias) = it.next() {
            if entry_or_alias.get_const_entry().has_codepoint(cp, p_mode) {
                return Some(Index {
                    style,
                    idx: i as IndexInt,
                });
            }

            i += 1;
        }

        // Not found
        None
    }

    /// Check if a specific font index has a specific codepoint. This does not
    /// necessarily force the font to load. The presentation value "p" will
    /// verify the Emoji representation matches if it is non-null. If "p" is
    /// null then any presentation will be accepted.
    pub fn has_codepoint(&self, index: Index, cp: u32, p_mode: PresentationMode) -> bool {
        let list = self.faces.get(index.style);
        if index.idx as usize >= list.count() {
            return false;
        }
        list.at(index.idx as usize)
            .get_const_entry()
            .has_codepoint(cp, p_mode)
    }

    /// Ensure we have an option for all styles in the collection, such
    /// as italic and bold by synthesizing them if necessary from the
    /// first regular face that has text glyphs.
    ///
    /// If there is no regular face that has text glyphs, then this
    /// does nothing.
    pub fn complete_styles(&mut self, synthetic_config: FontSyntheticStyle) -> Result<(), CompleteError> {
        // If every style has at least one entry then we're done!
        // This is the most common case.
        if Style::ALL.iter().all(|s| self.faces.get(*s).count() != 0) {
            return Ok(());
        }

        // Find the first regular face that has non-colorized text glyphs.
        // This is the font we want to fallback to. This may not be index zero
        // if a user configures something like an Emoji font first.
        let regular_entry: *mut Entry = 'entry: {
            let count = self.faces.get(Style::Regular).count();
            if count == 0 {
                return Ok(());
            }

            // Find our first regular face that has text glyphs.
            for i in 0..count {
                // Load our face. If we fail to load it, we just skip it and
                // continue on to try the next one.
                let entry: *mut Entry = self.faces.get_mut(Style::Regular).at_mut(i).get_entry();
                // SAFETY: entry points into our pointer-stable list.
                let face = match unsafe { self.get_face_from_entry(entry) } {
                    Ok(face) => face,
                    Err(err) => {
                        log::warn!(target: LOG, "error loading regular entry={} err={}", i, err);
                        continue;
                    }
                };

                // We have two conditionals here. The color check is obvious:
                // we want to auto-italicize a normal text font. The second
                // check is less obvious... for mixed color/non-color fonts, we
                // accept the regular font if it has basic ASCII. This may not
                // be strictly correct (especially with international fonts) but
                // it's a reasonable heuristic and the first case will match 99%
                // of the time.
                if !face.has_color() || face.glyph_index('A' as u32).is_some() {
                    break 'entry entry;
                }
            }

            // No regular text face found. We can't provide any fallback.
            return Err(CompleteError::DefaultUnavailable);
        };
        // SAFETY (for the alias pointers below): `regular_entry` points
        // into the regular list which is pointer-stable and never shrinks.
        let regular_alias = NonNull::new(regular_entry).unwrap();

        // If we don't have italic, attempt to create a synthetic italic face.
        // If we can't create a synthetic italic face, we'll just use the regular
        // face for italic.
        let have_italic = self.faces.get(Style::Italic).count() > 0;
        if !have_italic {
            'italic: {
                if !synthetic_config.italic {
                    log::info!(target: LOG, "italic style not available and synthetic italic disabled");
                    self.faces.get_mut(Style::Italic).append(EntryOrAlias::Alias(regular_alias));
                    break 'italic;
                }

                // SAFETY: see above.
                let synthetic = match unsafe { self.synthetic_italic(regular_entry) } {
                    Ok(v) => v,
                    Err(err) => {
                        log::warn!(target: LOG, "failed to create synthetic italic, italic style will not be available err={}", err);
                        self.faces.get_mut(Style::Italic).append(EntryOrAlias::Alias(regular_alias));
                        break 'italic;
                    }
                };

                let synthetic_entry = Entry::new(AnyFace::Loaded(synthetic), false);
                log::info!(target: LOG, "synthetic italic face created");
                self.faces.get_mut(Style::Italic).append(EntryOrAlias::Entry(synthetic_entry));
            }
        }

        // If we don't have bold, use the regular font.
        let have_bold = self.faces.get(Style::Bold).count() > 0;
        if !have_bold {
            'bold: {
                if !synthetic_config.bold {
                    log::info!(target: LOG, "bold style not available and synthetic bold disabled");
                    self.faces.get_mut(Style::Bold).append(EntryOrAlias::Alias(regular_alias));
                    break 'bold;
                }

                // SAFETY: see above.
                let synthetic = match unsafe { self.synthetic_bold(regular_entry) } {
                    Ok(v) => v,
                    Err(err) => {
                        log::warn!(target: LOG, "failed to create synthetic bold, bold style will not be available err={}", err);
                        self.faces.get_mut(Style::Bold).append(EntryOrAlias::Alias(regular_alias));
                        break 'bold;
                    }
                };

                let synthetic_entry = Entry::new(AnyFace::Loaded(synthetic), false);
                log::info!(target: LOG, "synthetic bold face created");
                self.faces.get_mut(Style::Bold).append(EntryOrAlias::Entry(synthetic_entry));
            }
        }

        // If we don't have bold italic, we attempt to synthesize a bold variant
        // of the italic font. If we can't do that, we'll use the italic font.
        if self.faces.get(Style::BoldItalic).count() == 0 {
            'bold_italic: {
                if !synthetic_config.bold_italic {
                    log::info!(target: LOG, "bold italic style not available and synthetic bold italic disabled");
                    self.faces.get_mut(Style::BoldItalic).append(EntryOrAlias::Alias(regular_alias));
                    break 'bold_italic;
                }

                // Prefer to synthesize on top of the face we already had. If we
                // have bold then we try to synthesize italic on top of bold.
                if have_bold {
                    let base_entry: *mut Entry = self.faces.get_mut(Style::Bold).at_mut(0).get_entry();
                    // SAFETY: points into our pointer-stable list.
                    if let Ok(synthetic) = unsafe { self.synthetic_italic(base_entry) } {
                        log::info!(target: LOG, "synthetic bold italic face created from bold");
                        let synthetic_entry = Entry::new(AnyFace::Loaded(synthetic), false);
                        self.faces
                            .get_mut(Style::BoldItalic)
                            .append(EntryOrAlias::Entry(synthetic_entry));
                        break 'bold_italic;
                    }

                    // If synthesizing italic failed, then we try to synthesize
                    // bold on whatever italic font we have.
                }

                let base_entry: *mut Entry = self.faces.get_mut(Style::Italic).at_mut(0).get_entry();
                // SAFETY: points into our pointer-stable list.
                if let Ok(synthetic) = unsafe { self.synthetic_bold(base_entry) } {
                    log::info!(target: LOG, "synthetic bold italic face created from italic");
                    let synthetic_entry = Entry::new(AnyFace::Loaded(synthetic), false);
                    self.faces
                        .get_mut(Style::BoldItalic)
                        .append(EntryOrAlias::Entry(synthetic_entry));
                    break 'bold_italic;
                }

                log::warn!(target: LOG, "bold italic style not available, using italic font");
                self.faces
                    .get_mut(Style::BoldItalic)
                    .append(EntryOrAlias::Alias(NonNull::new(base_entry).unwrap()));
            }
        }

        Ok(())
    }

    /// Create a synthetic bold font face from the given entry and return it.
    ///
    /// # Safety
    /// `entry` must point to a live entry of this collection.
    #[cfg(not(target_arch = "wasm32"))]
    #[allow(unused_assignments)]
    unsafe fn synthetic_bold(&mut self, entry: *mut Entry) -> Result<Face, SyntheticError> {
        // We require loading options to create a synthetic bold face.
        let Some(opts) = self.load_options.clone() else {
            return Err(SyntheticError::DeferredLoadingUnavailable);
        };

        // Try to bold it.
        let regular = self.get_face_from_entry(entry).map_err(SyntheticError::GetFace)?;

        // Inherit size from regular; it may be different than opts.size
        // due to scaling adjustments
        let mut face_opts = opts.face_options();
        face_opts.size = regular.size;
        // NOTE: like the Zig code, `opts.faceOptions()` (not `face_opts`)
        // is passed here.
        let face = regular
            .synthetic_bold(opts.face_options())
            .map_err(SyntheticError::Face)?;

        let mut buf = [0u8; 256];
        if let Ok(name) = face.name(&mut buf) {
            log::info!(target: LOG, "font synthetic bold created family={}", String::from_utf8_lossy(name));
        }

        Ok(face)
    }

    /// Not all font backends support synthetic bold.
    #[cfg(target_arch = "wasm32")]
    unsafe fn synthetic_bold(&mut self, _entry: *mut Entry) -> Result<Face, SyntheticError> {
        Err(SyntheticError::SyntheticBoldUnavailable)
    }

    /// Create a synthetic italic font face from the given entry and return it.
    ///
    /// # Safety
    /// `entry` must point to a live entry of this collection.
    #[cfg(not(target_arch = "wasm32"))]
    #[allow(unused_assignments)]
    unsafe fn synthetic_italic(&mut self, entry: *mut Entry) -> Result<Face, SyntheticError> {
        // We require loading options to create a synthetic italic face.
        let Some(opts) = self.load_options.clone() else {
            return Err(SyntheticError::DeferredLoadingUnavailable);
        };

        // Try to italicize it.
        let regular = self.get_face_from_entry(entry).map_err(SyntheticError::GetFace)?;

        // Inherit size from regular; it may be different than opts.size
        // due to scaling adjustments
        let mut face_opts = opts.face_options();
        face_opts.size = regular.size;
        // NOTE: like the Zig code, `opts.faceOptions()` (not `face_opts`)
        // is passed here.
        let face = regular
            .synthetic_italic(opts.face_options())
            .map_err(SyntheticError::Face)?;

        let mut buf = [0u8; 256];
        if let Ok(name) = face.name(&mut buf) {
            log::info!(target: LOG, "font synthetic italic created family={}", String::from_utf8_lossy(name));
        }

        Ok(face)
    }

    /// Not all font backends support synthetic italicization.
    #[cfg(target_arch = "wasm32")]
    unsafe fn synthetic_italic(&mut self, _entry: *mut Entry) -> Result<Face, SyntheticError> {
        Err(SyntheticError::SyntheticItalicUnavailable)
    }

    /// Update the size of all faces in the collection. This will
    /// also update the size in the load options for future deferred
    /// face loading.
    ///
    /// This requires load options to be set.
    pub fn set_size(&mut self, size: DesiredSize) -> Result<(), SetSizeError> {
        // Get a pointer to our options so we can modify the size.
        let Some(opts) = self.load_options.as_mut() else {
            return Err(SetSizeError::DeferredLoadingUnavailable);
        };
        opts.size = size;
        let opts = opts.clone();

        let primary_entry: Option<*const Entry> = self
            .get_entry(Index::default())
            .ok()
            .map(|e| e as *const Entry);

        // Resize all our faces that are loaded
        for style in Style::ALL {
            let list = self.faces.get_mut(style);
            // Resize all faces. We skip entries that are aliases, since
            // the underlying face will have a non-alias entry somewhere.
            for i in 0..list.count() {
                let entry_or_alias = list.at_mut(i);
                let EntryOrAlias::Entry(entry) = entry_or_alias else {
                    continue;
                };

                let entry_ptr = entry as *const Entry;
                let scale = entry.scale_factor;
                if let Some(face) = entry.get_loaded() {
                    // If this isn't our primary face, we scale
                    // the size appropriately before setting it.
                    //
                    // If we don't have a primary face we also don't.
                    let mut new_opts = opts.clone();
                    if primary_entry.is_some() && Some(entry_ptr) != primary_entry {
                        new_opts.size.points *= scale.scale() as f32;
                    }
                    face.set_size(new_opts.face_options())
                        .map_err(|_| SetSizeError::SetSizeFailed)?;
                }
            }
        }

        self.update_metrics()?;
        Ok(())
    }

    /// Calculate a factor by which to scale the provided face to match
    /// it with the primary face, depending on the specified adjustment.
    ///
    /// If this encounters any problems loading the primary face or its
    /// metrics then it just returns `1.0`.
    ///
    /// This functions very much like the `font-size-adjust` CSS property.
    /// ref: https://developer.mozilla.org/en-US/docs/Web/CSS/font-size-adjust
    fn scale_factor(&mut self, face_metrics: FaceMetrics, adjustment: SizeAdjustment) -> f64 {
        // If there's no adjustment, the scale is 1.0
        if adjustment == SizeAdjustment::None {
            return 1.0;
        }

        // If we haven't calculated our primary face metrics yet, do so now.
        if self.primary_face_metrics.is_none() {
            // If we can't load the primary face, just use 1.0 as the scale factor.
            let Ok(primary_face) = self.get_face(Index::default()) else {
                return 1.0;
            };
            self.primary_face_metrics = Some(primary_face.get_metrics());
        }

        let primary_metrics = self.primary_face_metrics.unwrap();

        // We normalize the metrics values which are expressed in px to instead
        // be in ems, so that it doesn't matter what size the faces actually are.
        let primary_scale = 1.0 / primary_metrics.px_per_em;
        let face_scale = 1.0 / face_metrics.px_per_em;

        // We get the target metrics from the primary face and this face depending
        // on the specified `adjustment`. If this face doesn't explicitly define a
        // metric, or if the value it defines is invalid, we fall through to other
        // options in the order below.
        //
        // In order to make sure the value is valid, we compare it with the result
        // of the estimator function, which rules out both null and invalid values.
        let mut normalize_by = adjustment;
        let (primary_metric, face_metric): (f64, f64) = loop {
            match normalize_by {
                SizeAdjustment::IcWidth => {
                    if face_metrics.ic_width != Some(face_metrics.ic_width()) {
                        normalize_by = SizeAdjustment::ExHeight;
                        continue;
                    }

                    break (
                        primary_metrics.ic_width() * primary_scale,
                        face_metrics.ic_width() * face_scale,
                    );
                }

                SizeAdjustment::ExHeight => {
                    if face_metrics.ex_height != Some(face_metrics.ex_height()) {
                        normalize_by = SizeAdjustment::CapHeight;
                        continue;
                    }

                    break (
                        primary_metrics.ex_height() * primary_scale,
                        face_metrics.ex_height() * face_scale,
                    );
                }

                SizeAdjustment::CapHeight => {
                    if face_metrics.cap_height != Some(face_metrics.cap_height()) {
                        normalize_by = SizeAdjustment::LineHeight;
                        continue;
                    }

                    break (
                        primary_metrics.cap_height() * primary_scale,
                        face_metrics.cap_height() * face_scale,
                    );
                }

                SizeAdjustment::LineHeight => {
                    break (
                        primary_metrics.line_height() * primary_scale,
                        face_metrics.line_height() * face_scale,
                    );
                }

                SizeAdjustment::None => unreachable!(),
            }
        };

        primary_metric / face_metric
    }

    /// Update the cell metrics for this collection, based on
    /// the primary font and the modifiers in `metric_modifiers`.
    ///
    /// This requires a primary font (index `0`) to be present.
    pub fn update_metrics(&mut self) -> Result<(), UpdateMetricsError> {
        let primary_face = self
            .get_face(Index::default())
            .map_err(|_| UpdateMetricsError::CannotLoadPrimaryFont)?;

        let face_metrics = primary_face.get_metrics();
        self.primary_face_metrics = Some(face_metrics);

        let mut metrics = Metrics::calc(face_metrics);

        metrics.apply(&self.metric_modifiers);

        self.metrics = Some(metrics);
        Ok(())
    }
}

/// Options for adjusting the size of a face relative to the primary face.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SizeAdjustment {
    /// Don't adjust the size for this face, use the original point size.
    None,
    /// Match ideograph character width with the primary face.
    IcWidth,
    /// Match ex height with the primary face.
    ExHeight,
    /// Match cap height with the primary face.
    CapHeight,
    /// Match line height with the primary face.
    LineHeight,
}

/// The segmented list type for one style (Zig
/// `SegmentedList(EntryOrAlias, 0)`).
pub type StyleList = SegmentedList<EntryOrAlias, 0>;

/// Packed array of all Style enum cases mapped to a growable list of faces.
///
/// We use this data structure because there aren't many styles and all
/// styles are typically loaded for a terminal session. The overhead per
/// style even if it is not used or barely used is minimal given the
/// small style count.
///
/// We use a segmented list because the entry values must be pointer-stable
/// to support aliases.
///
/// WARNING: We cannot use any prealloc yet for the segmented list because
/// the collection is copied around by value and pointers aren't stable.
pub struct StyleArray {
    pub values: [StyleList; 4],
}

impl StyleArray {
    /// Zig `initFill(.{})`.
    pub fn init_fill() -> StyleArray {
        StyleArray {
            values: [StyleList::new(), StyleList::new(), StyleList::new(), StyleList::new()],
        }
    }

    pub fn get(&self, style: Style) -> &StyleList {
        &self.values[style as usize]
    }

    pub fn get_mut(&mut self, style: Style) -> &mut StyleList {
        &mut self.values[style as usize]
    }

    /// Zig `getPtr`.
    pub fn get_ptr(&mut self, style: Style) -> &mut StyleList {
        self.get_mut(style)
    }
}

/// Load options are used to configure all the details a Collection
/// needs to load deferred faces.
#[derive(Debug, Clone)]
pub struct LoadOptions {
    /// The library to use for loading faces. This is not owned by
    /// the collection and can be used by multiple collections. When
    /// deinitializing the collection, the library is not deinitialized.
    pub library: Library,

    /// The desired font size for all loaded faces.
    pub size: DesiredSize,

    /// Freetype Load Flags to use when loading glyphs. This is a list of
    /// bitfield constants that controls operations to perform during glyph
    /// loading. Only a subset is exposed for configuration, for the whole set
    /// of flags see `pkg.freetype.face.LoadFlags`.
    pub freetype_load_flags: FreetypeLoadFlags,
}

impl LoadOptions {
    /// Load options with the Zig field defaults.
    pub fn new(library: Library) -> LoadOptions {
        LoadOptions {
            library,
            size: DesiredSize::new(12.0),
            freetype_load_flags: face::freetype_load_flags_default(),
        }
    }

    /// The options to use for loading faces.
    pub fn face_options(&self) -> face::Options {
        face::Options {
            size: self.size,
            freetype_load_flags: self.freetype_load_flags,
        }
    }
}

/// Zig `Entry.AnyFace`.
pub enum AnyFace {
    /// Not yet loaded.
    Deferred(DeferredFace),

    /// Loaded.
    Loaded(Face),
}

/// Zig: the anonymous `scale_factor` union of `Entry`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScaleFactor {
    Adjustment(SizeAdjustment),
    Scale(f64),
}

impl ScaleFactor {
    /// Zig `entry.scale_factor.scale` (the active field must be `scale`).
    pub fn scale(self) -> f64 {
        match self {
            ScaleFactor::Scale(v) => v,
            ScaleFactor::Adjustment(_) => unreachable!("scale_factor is not a scale"),
        }
    }
}

impl Default for ScaleFactor {
    fn default() -> ScaleFactor {
        ScaleFactor::Scale(1.0)
    }
}

/// A entry in a collection can be deferred or loaded. A deferred face
/// is not yet fully loaded and only represents the font descriptor
/// and usually uses less resources. A loaded face is fully parsed,
/// ready to rasterize, and usually uses more resources than a
/// deferred version.
///
/// A face can also be a "fallback" variant that is still either
/// deferred or loaded. Today, there is only one difference between
/// fallback and non-fallback (or "explicit") faces: the handling
/// of emoji presentation.
///
/// For explicit faces, when an explicit emoji presentation is
/// not requested, we will use any glyph for that codepoint found
/// even if the font presentation does not match the UCD
/// (Unicode Character Database) value. When an explicit presentation
/// is requested (via either VS15/V16), that is always honored.
/// The reason we do this is because we assume that if a user
/// explicitly chosen a font face (hence it is "explicit" and
/// not "fallback"), they want to use any glyphs possible within that
/// font face. Fallback fonts on the other hand are picked as a
/// last resort, so we should prefer exactness if possible.
pub struct Entry {
    pub face: AnyFace,

    /// Whether this face is a fallback face, see
    /// main doc comment on Entry for more info.
    pub fallback: bool,

    /// Factor to multiply the collection size by for this face, or
    /// else the size adjustment that should be used to calculate
    /// once the face is loaded.
    ///
    /// This is computed when the face is loaded, based on a scale
    /// factor computed for an adjustment from the primary face to
    /// this one, which allows fallback fonts to be harmonized with
    /// the primary font by matching one of the metrics between them.
    pub scale_factor: ScaleFactor,
}

impl Entry {
    /// An entry with the default scale factor (`.{ .scale = 1.0 }`).
    pub fn new(face: AnyFace, fallback: bool) -> Entry {
        Entry {
            face,
            fallback,
            scale_factor: ScaleFactor::default(),
        }
    }

    /// If this face is loaded, then this returns the `Face`,
    /// otherwise returns null.
    pub fn get_loaded(&mut self) -> Option<&mut Face> {
        match &mut self.face {
            AnyFace::Deferred(_) => None,
            AnyFace::Loaded(face) => Some(face),
        }
    }

    /// True if the entry is deferred.
    #[allow(dead_code)]
    fn is_deferred(&self) -> bool {
        match self.face {
            AnyFace::Deferred(_) => true,
            AnyFace::Loaded(_) => false,
        }
    }

    /// True if this face satisfies the given codepoint and presentation.
    pub fn has_codepoint(&self, cp: u32, p_mode: PresentationMode) -> bool {
        let mut mode = p_mode;
        loop {
            match mode {
                PresentationMode::Default(p) => {
                    mode = if self.fallback {
                        // Fallback fonts require explicit presentation matching.
                        PresentationMode::Explicit(p)
                    } else {
                        // Non-fallback fonts do not.
                        PresentationMode::Any
                    };
                    continue;
                }

                PresentationMode::Explicit(p) => {
                    return match &self.face {
                        AnyFace::Deferred(v) => v.has_codepoint(cp, Some(p)),

                        AnyFace::Loaded(face) => {
                            let Some(index) = face.glyph_index(cp) else {
                                return false;
                            };
                            match p {
                                Presentation::Text => !face.is_color_glyph(index),
                                Presentation::Emoji => face.is_color_glyph(index),
                            }
                        }
                    };
                }

                PresentationMode::Any => {
                    return match &self.face {
                        AnyFace::Deferred(v) => v.has_codepoint(cp, None),
                        AnyFace::Loaded(face) => face.glyph_index(cp).is_some(),
                    };
                }
            }
        }
    }
}

pub enum EntryOrAlias {
    Entry(Entry),

    /// An alias to another entry. This is used to share the same face,
    /// avoid memory duplication. An alias must point to a non-alias entry.
    ///
    /// The pointer targets an entry of the same collection (whose lists
    /// are pointer-stable).
    Alias(NonNull<Entry>),
}

impl EntryOrAlias {
    /// Get a pointer to the underlying entry.
    pub fn get_entry(&mut self) -> &mut Entry {
        match self {
            EntryOrAlias::Entry(v) => v,
            // SAFETY: aliases point to live entries of the same collection.
            EntryOrAlias::Alias(v) => unsafe { v.as_mut() },
        }
    }

    /// Get a const pointer to the underlying entry.
    pub fn get_const_entry(&self) -> &Entry {
        match self {
            EntryOrAlias::Entry(v) => v,
            // SAFETY: aliases point to live entries of the same collection.
            EntryOrAlias::Alias(v) => unsafe { v.as_ref() },
        }
    }
}

/// The requested presentation for a codepoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentationMode {
    /// The codepoint has an explicit presentation that is required,
    /// i.e. VS15/V16.
    Explicit(Presentation),

    /// The codepoint has no explicit presentation and we should use
    /// the presentation from the UCD.
    Default(Presentation),

    /// The codepoint can be any presentation.
    Any,
}

/// The backing integer of [`Index`] (Zig `Index.Backing`).
pub type IndexBacking = u16;

/// The number of bits we use for the index.
pub const INDEX_IDX_BITS: u32 = IndexBacking::BITS - 3;

/// Zig `Index.IndexInt` (a `u13`).
pub type IndexInt = u16;

/// The special-case fonts that we support. (Zig `Index.Special`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum Special {
    /// Sprite drawing, this is rendered JIT using 2D graphics APIs.
    Sprite = SPECIAL_START,
}

// We start all special fonts at this index so they can be detected.
const SPECIAL_START: IndexInt = (1 << INDEX_IDX_BITS) - 1;

impl Special {
    // We start all special fonts at this index so they can be detected.
    pub const START: IndexInt = SPECIAL_START;
}

/// This represents a specific font in the collection.
///
/// The backing size of this packed struct represents the total number
/// of possible usable fonts in a collection. And the number of bits
/// used for the index and not the style represents the total number
/// of possible usable fonts for a given style.
///
/// The goal is to keep the size of this struct as small as practical. We
/// accept the limitations that this imposes so long as they're reasonable.
/// At the time of writing this comment, this is a 16-bit struct with 13
/// bits used for the index, supporting up to 8192 fonts per style. This
/// seems more than reasonable. There are synthetic scenarios where this
/// could be a limitation but I can't think of any that are practical.
///
/// If you somehow need more fonts per style, you can increase the size of
/// the Backing type and everything should just work fine.
///
/// (Zig `packed struct(u16)`: `style` in the low 3 bits, `idx` in the
/// high 13 bits; see [`Index::int`].)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Index {
    pub style: Style,
    /// Only the low 13 bits are used.
    pub idx: IndexInt,
}

impl Index {
    /// Initialize a special font index.
    pub const fn init_special(v: Special) -> Index {
        Index {
            style: Style::Regular,
            idx: v as IndexInt,
        }
    }

    /// Convert to int
    pub const fn int(self) -> IndexBacking {
        (self.style as IndexBacking) | (self.idx << 3)
    }

    /// Inverse of [`Index::int`] (Zig `@bitCast`).
    pub const fn from_int(v: IndexBacking) -> Index {
        Index {
            style: match Style::from_int((v & 0b111) as u8) {
                Some(s) => s,
                None => Style::Regular,
            },
            idx: v >> 3,
        }
    }

    /// Returns true if this is a "special" index which doesn't map to
    /// a real font face. We can still render it but there is no face for
    /// this font.
    pub fn special(self) -> Option<Special> {
        if self.idx < Special::START {
            return None;
        }
        Some(Special::Sprite)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datastruct::comparison::expect_approx_equal;
    use crate::font::backend::Backend;
    use crate::font::embedded;

    crate::impl_approx_equal_fields!(Metrics {
        cell_width,
        cell_height,
        cell_baseline,
        underline_position,
        underline_thickness,
        strikethrough_position,
        strikethrough_thickness,
        overline_position,
        overline_thickness,
        box_thickness,
        cursor_thickness,
        cursor_height,
        icon_height,
        icon_height_single,
        face_width,
        face_height,
        face_y,
    });

    crate::impl_approx_equal_fields!(FaceMetrics {
        px_per_em,
        cell_width,
        ascent,
        descent,
        line_gap,
        underline_position,
        underline_thickness,
        strikethrough_position,
        strikethrough_thickness,
        cap_height,
        ex_height,
        ascii_height,
        ic_width,
    });

    fn size96(points: f32) -> DesiredSize {
        DesiredSize {
            points,
            xdpi: 96,
            ydpi: 96,
        }
    }

    fn opts(style: Style, size_adjustment: SizeAdjustment) -> AddOptions {
        AddOptions {
            style,
            fallback: false,
            size_adjustment,
        }
    }

    #[test]
    fn index() {
        // We never want to take up more than a byte since font indexes are
        // everywhere so if we increase the size of this we'll dramatically
        // increase our memory usage.
        assert_eq!(std::mem::size_of::<IndexBacking>(), 2);
        assert_eq!(Index::from_int(Index { style: Style::Italic, idx: 42 }.int()), Index { style: Style::Italic, idx: 42 });

        // Just so we're aware when this changes. The current maximum number
        // of fonts for a style is 13 bits or 8192 fonts.
        assert_eq!(13, INDEX_IDX_BITS);
    }

    #[test]
    fn init() {
        let _c = Collection::init();
    }

    #[test]
    fn add_full() {
        let test_font = embedded::REGULAR;

        let mut lib = Library::init().unwrap();

        {
            let mut c = Collection::init();

            for _ in 0..(Special::START - 1) {
                c.add(
                    Face::init(lib.clone(), test_font, DesiredSize::new(12.0).into()).unwrap(),
                    opts(Style::Regular, SizeAdjustment::None),
                )
                .unwrap();
            }

            let face = Face::init(lib.clone(), test_font, DesiredSize::new(12.0).into()).unwrap();
            assert_eq!(
                Err(AddError::CollectionFull),
                c.add(face, opts(Style::Regular, SizeAdjustment::None))
            );
        }

        lib.deinit();
    }

    #[test]
    fn add_deferred_without_loading_options() {
        let mut c = Collection::init();

        // The Zig test passes `undefined`, which is never accessed; Rust
        // needs a real value, so use an empty deferred face.
        let face = DeferredFace::default();
        assert_eq!(
            Err(AddDeferredError::DeferredLoadingUnavailable),
            c.add_deferred(face, opts(Style::Regular, SizeAdjustment::None))
        );
    }

    #[test]
    fn get_face() {
        let test_font = embedded::REGULAR;

        let mut lib = Library::init().unwrap();
        {
            let mut c = Collection::init();

            let idx = c
                .add(
                    Face::init(lib.clone(), test_font, size96(12.0).into()).unwrap(),
                    opts(Style::Regular, SizeAdjustment::None),
                )
                .unwrap();

            {
                let face1 = c.get_face(idx).unwrap() as *const Face;
                let face2 = c.get_face(idx).unwrap() as *const Face;
                assert_eq!(face1, face2);
            }
        }
        lib.deinit();
    }

    #[test]
    fn get_index() {
        let test_font = embedded::REGULAR;

        let mut lib = Library::init().unwrap();
        {
            let mut c = Collection::init();

            c.add(
                Face::init(lib.clone(), test_font, size96(12.0).into()).unwrap(),
                opts(Style::Regular, SizeAdjustment::None),
            )
            .unwrap();

            // Should find all visible ASCII
            for i in 32u32..127 {
                let idx = c.get_index(i, Style::Regular, PresentationMode::Any);
                assert!(idx.is_some());
            }

            // Should not find emoji
            {
                let idx = c.get_index('🥸' as u32, Style::Regular, PresentationMode::Any);
                assert!(idx.is_none());
            }
        }
        lib.deinit();
    }

    #[test]
    fn complete_styles() {
        let test_font = embedded::REGULAR;

        let mut lib = Library::init().unwrap();
        {
            let mut c = Collection::init();
            c.load_options = Some(LoadOptions::new(lib.clone()));

            c.add(
                Face::init(lib.clone(), test_font, size96(12.0).into()).unwrap(),
                opts(Style::Regular, SizeAdjustment::None),
            )
            .unwrap();

            assert!(c.get_index('A' as u32, Style::Bold, PresentationMode::Any).is_none());
            assert!(c.get_index('A' as u32, Style::Italic, PresentationMode::Any).is_none());
            assert!(c.get_index('A' as u32, Style::BoldItalic, PresentationMode::Any).is_none());
            c.complete_styles(FontSyntheticStyle::default()).unwrap();
            assert!(c.get_index('A' as u32, Style::Bold, PresentationMode::Any).is_some());
            assert!(c.get_index('A' as u32, Style::Italic, PresentationMode::Any).is_some());
            assert!(c.get_index('A' as u32, Style::BoldItalic, PresentationMode::Any).is_some());
        }
        lib.deinit();
    }

    #[test]
    fn set_size() {
        let test_font = embedded::REGULAR;

        let mut lib = Library::init().unwrap();
        {
            let mut c = Collection::init();
            c.load_options = Some(LoadOptions::new(lib.clone()));

            c.add(
                Face::init(lib.clone(), test_font, size96(12.0).into()).unwrap(),
                opts(Style::Regular, SizeAdjustment::None),
            )
            .unwrap();

            assert_eq!(12.0, c.load_options.as_ref().unwrap().size.points);
            c.set_size(DesiredSize::new(24.0)).unwrap();
            assert_eq!(24.0, c.load_options.as_ref().unwrap().size.points);
        }
        lib.deinit();
    }

    #[test]
    fn has_codepoint() {
        let test_font = embedded::REGULAR;

        let mut lib = Library::init().unwrap();
        {
            let mut c = Collection::init();
            c.load_options = Some(LoadOptions::new(lib.clone()));

            let idx = c
                .add(
                    Face::init(lib.clone(), test_font, size96(12.0).into()).unwrap(),
                    opts(Style::Regular, SizeAdjustment::None),
                )
                .unwrap();

            assert!(c.has_codepoint(idx, 'A' as u32, PresentationMode::Any));
            assert!(!c.has_codepoint(idx, '🥸' as u32, PresentationMode::Any));
        }
        lib.deinit();
    }

    #[test]
    fn has_codepoint_emoji_default_graphical() {
        if crate::font::OPTIONS.backend != Backend::FontconfigFreetype {
            return;
        }

        let test_emoji = embedded::EMOJI;

        let mut lib = Library::init().unwrap();
        {
            let mut c = Collection::init();
            c.load_options = Some(LoadOptions::new(lib.clone()));

            let idx = c
                .add(
                    Face::init(lib.clone(), test_emoji, size96(12.0).into()).unwrap(),
                    opts(Style::Regular, SizeAdjustment::None),
                )
                .unwrap();

            assert!(!c.has_codepoint(idx, 'A' as u32, PresentationMode::Any));
            assert!(c.has_codepoint(idx, '🥸' as u32, PresentationMode::Any));
            // TODO(fontmem): test explicit/implicit
        }
        lib.deinit();
    }

    #[test]
    fn metrics() {
        let test_font = embedded::INCONSOLATA;

        let mut lib = Library::init().unwrap();
        {
            let mut c = Collection::init();
            let size = size96(12.0);
            let mut lo = LoadOptions::new(lib.clone());
            lo.size = size;
            c.load_options = Some(lo);

            c.add(
                Face::init(lib.clone(), test_font, size.into()).unwrap(),
                opts(Style::Regular, SizeAdjustment::None),
            )
            .unwrap();

            c.update_metrics().unwrap();

            expect_approx_equal(
                &Some(Metrics {
                    cell_width: 8,
                    // The cell height is 17 px because the calculation is
                    //
                    //  ascender - descender + gap
                    //
                    // which, for inconsolata is
                    //
                    //  859 - -190 + 0
                    //
                    // font units, at 1000 units per em that works out to 1.049 em,
                    // and 1em should be the point size * dpi scale, so 12 * (96/72)
                    // which is 16, and 16 * 1.049 = 16.784, which finally is rounded
                    // to 17.
                    //
                    // The icon height is (2 * cap_height + face_height) / 3
                    // = (2 * 623 + 1049) / 3 = 765, and 16 * 0.765 = 12.24.
                    cell_height: 17,
                    cell_baseline: 3,
                    underline_position: 17,
                    underline_thickness: 1,
                    strikethrough_position: 10,
                    strikethrough_thickness: 1,
                    overline_position: 0,
                    overline_thickness: 1,
                    box_thickness: 1,
                    cursor_thickness: 1,
                    cursor_height: 17,
                    icon_height: 16.784,
                    icon_height_single: 12.24,
                    face_width: 8.0,
                    face_height: 16.784,
                    face_y: -0.04,
                }),
                &c.metrics,
            )
            .unwrap();

            // Resize should change metrics
            c.set_size(size96(24.0)).unwrap();
            expect_approx_equal(
                &Some(Metrics {
                    cell_width: 16,
                    cell_height: 34,
                    cell_baseline: 6,
                    underline_position: 34,
                    underline_thickness: 2,
                    strikethrough_position: 19,
                    strikethrough_thickness: 2,
                    overline_position: 0,
                    overline_thickness: 2,
                    box_thickness: 2,
                    cursor_thickness: 1,
                    cursor_height: 34,
                    icon_height: 33.568,
                    icon_height_single: 24.48,
                    face_width: 16.0,
                    face_height: 33.568,
                    face_y: -0.08,
                }),
                &c.metrics,
            )
            .unwrap();
        }
        lib.deinit();
    }

    // TODO: Also test CJK fallback sizing, we don't currently have a CJK test font.
    #[test]
    fn adjusted_sizes() {
        let test_font = embedded::INCONSOLATA;
        let fallback = embedded::MONASPACE_NEON;
        let symbol = embedded::SYMBOLS_NERD_FONT;

        let mut lib = Library::init().unwrap();
        {
            let mut c = Collection::init();
            let size = size96(12.0);
            let mut lo = LoadOptions::new(lib.clone());
            lo.size = size;
            c.load_options = Some(lo);

            // Add our primary face.
            c.add(
                Face::init(lib.clone(), test_font, size.into()).unwrap(),
                opts(Style::Regular, SizeAdjustment::None),
            )
            .unwrap();

            c.update_metrics().unwrap();

            type Getter = fn(&FaceMetrics) -> Option<f64>;
            let metrics_list: [(SizeAdjustment, Getter); 2] = [
                (SizeAdjustment::ExHeight, |m| m.ex_height),
                (SizeAdjustment::CapHeight, |m| m.cap_height),
            ];
            for (adjustment, metric) in metrics_list {
                // Add the fallback face with the chosen adjustment metric.
                let fallback_idx = c
                    .add(
                        Face::init(lib.clone(), fallback, size.into()).unwrap(),
                        opts(Style::Regular, adjustment),
                    )
                    .unwrap();

                // The chosen metric should match.
                {
                    let primary_metrics = c.get_face(Index::default()).unwrap().get_metrics();
                    let fallback_metrics = c.get_face(fallback_idx).unwrap().get_metrics();

                    // We accept anything within half a pixel.
                    assert!(
                        (metric(&primary_metrics).unwrap() - metric(&fallback_metrics).unwrap()).abs()
                            <= 0.5
                    );
                }

                // Resize should keep that relationship.
                c.set_size(size96(37.0)).unwrap();
                {
                    let primary_metrics = c.get_face(Index::default()).unwrap().get_metrics();
                    let fallback_metrics = c.get_face(fallback_idx).unwrap().get_metrics();

                    // We accept anything within half a pixel.
                    assert!(
                        (metric(&primary_metrics).unwrap() - metric(&fallback_metrics).unwrap()).abs()
                            <= 0.5
                    );
                }

                // Reset size for the next iteration
                c.set_size(size).unwrap();
            }

            {
                // A reference metric of "none" should leave the size unchanged.
                let fallback_idx = c
                    .add(
                        Face::init(lib.clone(), fallback, size.into()).unwrap(),
                        opts(Style::Regular, SizeAdjustment::None),
                    )
                    .unwrap();

                {
                    let primary_points = c.get_face(Index::default()).unwrap().size.points;
                    assert_eq!(primary_points, c.get_face(fallback_idx).unwrap().size.points);
                }

                // Resize should keep that.
                c.set_size(size96(37.0)).unwrap();

                {
                    let primary_points = c.get_face(Index::default()).unwrap().size.points;
                    assert_eq!(primary_points, c.get_face(fallback_idx).unwrap().size.points);
                }

                // Reset collection size
                c.set_size(size).unwrap();
            }

            // Add the symbol face.
            let symbol_idx = c
                .add(
                    Face::init(lib.clone(), symbol, size.into()).unwrap(),
                    opts(Style::Regular, SizeAdjustment::ExHeight),
                )
                .unwrap();

            // Test fallback to lineHeight() (ex_height and cap_height not defined in symbols font).
            {
                let primary_metrics = c.get_face(Index::default()).unwrap().get_metrics();
                let symbol_metrics = c.get_face(symbol_idx).unwrap().get_metrics();

                // We accept anything within half a pixel.
                assert!((primary_metrics.line_height() - symbol_metrics.line_height()).abs() <= 0.5);
            }
        }
        lib.deinit();
    }

    #[test]
    fn face_metrics() {
        // The web canvas backend doesn't calculate face metrics, only cell metrics
        if crate::font::OPTIONS.backend != Backend::WebCanvas {
            return;
        }

        let narrow_font = embedded::COZETTE;
        let wide_font = embedded::GEIST_MONO;

        let mut lib = Library::init().unwrap();
        {
            let mut c = Collection::init();
            let size = size96(12.0);
            let mut lo = LoadOptions::new(lib.clone());
            lo.size = size;
            c.load_options = Some(lo);

            let narrow_index = c
                .add(
                    Face::init(lib.clone(), narrow_font, size.into()).unwrap(),
                    opts(Style::Regular, SizeAdjustment::None),
                )
                .unwrap();
            let wide_index = c
                .add(
                    Face::init(lib.clone(), wide_font, size.into()).unwrap(),
                    opts(Style::Regular, SizeAdjustment::None),
                )
                .unwrap();

            let narrow_metrics: FaceMetrics = c.get_face(narrow_index).unwrap().get_metrics();
            let wide_metrics: FaceMetrics = c.get_face(wide_index).unwrap().get_metrics();

            let backend = crate::font::OPTIONS.backend;
            let is_ft = matches!(
                backend,
                Backend::Freetype | Backend::FontconfigFreetype | Backend::CoretextFreetype
            );
            let is_ct = matches!(
                backend,
                Backend::Coretext | Backend::CoretextHarfbuzz | Backend::CoretextNoshape
            );
            let pick = |ft: f64, ct: f64| -> f64 {
                if is_ft {
                    ft
                } else if is_ct {
                    ct
                } else {
                    unreachable!()
                }
            };

            // Verify provided/measured metrics. Measured
            // values are backend-dependent due to hinting.
            let narrow_metrics_expected = FaceMetrics {
                px_per_em: 16.0,
                cell_width: pick(8.0, 7.3828125),
                ascent: 12.3046875,
                descent: -3.6953125,
                line_gap: 0.0,
                underline_position: Some(-1.2265625),
                underline_thickness: Some(1.2265625),
                strikethrough_position: Some(6.15625),
                strikethrough_thickness: Some(1.234375),
                cap_height: Some(9.84375),
                ex_height: Some(7.3828125),
                ascii_height: Some(pick(18.0625, 16.0)),
                ic_width: None,
            };
            let wide_metrics_expected = FaceMetrics {
                px_per_em: 16.0,
                cell_width: pick(10.0, 9.6),
                ascent: 14.72,
                descent: -3.52,
                line_gap: 1.6,
                underline_position: Some(-1.6),
                underline_thickness: Some(0.8),
                strikethrough_position: Some(4.24),
                strikethrough_thickness: Some(0.8),
                cap_height: Some(11.36),
                ex_height: Some(8.48),
                ascii_height: Some(pick(16.0, 15.472000000000001)),
                ic_width: None,
            };

            for (metrics_expected, metrics_actual) in [
                (narrow_metrics_expected, narrow_metrics),
                (wide_metrics_expected, wide_metrics),
            ] {
                expect_approx_equal(&metrics_expected, &metrics_actual).unwrap();
            }

            // Verify estimated metrics. icWidth() should equal the smaller of
            // 2 * cell_width and ascii_height. For a narrow (wide) font, the
            // smaller quantity is the former (latter).
            assert_eq!(2.0 * narrow_metrics.cell_width, narrow_metrics.ic_width());
            assert_eq!(wide_metrics.ascii_height.unwrap(), wide_metrics.ic_width());
        }
        lib.deinit();
    }
}
