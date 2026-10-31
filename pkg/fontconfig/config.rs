//! Port of `pkg/fontconfig/config.zig`.
//!
//! STATUS: ported

use std::ptr::NonNull;

use super::c;
use super::char_set::CharSet;
use super::common::{MatchKind, Result};
use super::error::Error;
use super::font_set::FontSet;
use super::object_set::ObjectSet;
use super::pattern::Pattern;

/// Zig: `opaque` used as `*Config`. A copyable, non-null handle to an
/// `FcConfig`; it must be released explicitly with [`Config::destroy`].
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config(NonNull<c::FcConfig>);

impl Config {
    /// # Safety
    /// The handle (and its copies) must not be used afterwards.
    pub unsafe fn destroy(self) {
        c::FcConfigDestroy(self.cval());
    }

    pub fn font_list(self, pat: Pattern, os: ObjectSet) -> FontSet {
        // SAFETY: valid handles.
        unsafe { FontSet::from_raw(c::FcFontList(self.cval(), pat.cval(), os.cval())) }
    }

    /// `charset`, if given, receives the union of the character sets of
    /// the returned fonts (Zig: `?[*]*CharSet`).
    pub fn font_sort(
        self,
        pat: Pattern,
        trim: bool,
        charset: Option<&mut Option<CharSet>>,
    ) -> FontSortResult {
        let mut result: c::FcResult = c::FcResultMatch;
        // SAFETY: valid handles; Option<CharSet> has the layout of a
        // nullable `FcCharSet*`.
        let fs = unsafe {
            c::FcFontSort(
                self.cval(),
                pat.cval(),
                if trim { c::FcTrue } else { c::FcFalse } as c::FcBool,
                charset.map_or(std::ptr::null_mut(), |cs| {
                    cs as *mut Option<CharSet> as *mut *mut c::FcCharSet
                }),
                &mut result,
            )
        };

        FontSortResult {
            result: Result::from_raw(result),
            // SAFETY: FcFontSort returned a font set.
            fs: unsafe { FontSet::from_raw(fs) },
        }
    }

    pub fn font_render_prepare(
        self,
        pat: Pattern,
        font: Pattern,
    ) -> std::result::Result<Pattern, Error> {
        // SAFETY: valid handles.
        let ptr = unsafe { c::FcFontRenderPrepare(self.cval(), pat.cval(), font.cval()) };
        if ptr.is_null() {
            return Err(Error::FontconfigFailed);
        }
        // SAFETY: non-null pattern returned by fontconfig.
        Ok(unsafe { Pattern::from_raw(ptr) })
    }

    pub fn substitute_with_pat(self, pat: Pattern, kind: MatchKind) -> bool {
        // SAFETY: valid handles.
        unsafe {
            c::FcConfigSubstitute(self.cval(), pat.cval(), kind as c::FcMatchKind)
                == c::FcTrue as c::FcBool
        }
    }

    /// Wraps a raw, non-null `FcConfig` pointer.
    ///
    /// # Safety
    /// `ptr` must be a valid `FcConfig`.
    pub unsafe fn from_raw(ptr: *mut c::FcConfig) -> Config {
        Config(NonNull::new(ptr).expect("null FcConfig"))
    }

    #[inline]
    pub fn cval(self) -> *mut c::_FcConfig {
        self.0.as_ptr()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct FontSortResult {
    pub result: Result,
    pub fs: FontSet,
}
