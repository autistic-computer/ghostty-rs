//! Port of `src/terminal/c/row.zig`.
//!
//! STATUS: ported

use std::ffi::{c_int, c_void};

use super::result::Result;
use crate::terminal::page::{Row, RowC};

/// C: GhosttyRow
pub type CRow = RowC;

crate::c_api_enum! {
    /// C: GhosttyRowSemanticPrompt
    pub enum SemanticPrompt {
        None = 0,
        Prompt = 1,
        PromptContinuation = 2,
    }
}

crate::c_api_enum! {
    /// C: GhosttyRowData
    pub enum RowData {
        Invalid = 0,

        /// Whether this row is soft-wrapped.
        /// Output type: bool *
        Wrap = 1,

        /// Whether this row is a continuation of a soft-wrapped row.
        /// Output type: bool *
        WrapContinuation = 2,

        /// Whether any cells in this row have grapheme clusters.
        /// Output type: bool *
        Grapheme = 3,

        /// Whether any cells in this row have styling (may have false positives).
        /// Output type: bool *
        Styled = 4,

        /// Whether any cells in this row have hyperlinks (may have false positives).
        /// Output type: bool *
        Hyperlink = 5,

        /// The semantic prompt state of this row.
        /// Output type: GhosttyRowSemanticPrompt *
        SemanticPrompt = 6,

        /// Whether this row contains a Kitty virtual placeholder.
        /// Output type: bool *
        KittyVirtualPlaceholder = 7,

        /// Whether this row is dirty and requires a redraw.
        /// Output type: bool *
        Dirty = 8,
    }
}

/// C: ghostty_row_get
///
/// # Safety
/// `out` must point to storage of the output type for `data`.
#[export_name = "ghostty_row_get"]
pub unsafe extern "C" fn get(row_: CRow, data: c_int, out: *mut c_void) -> Result {
    let Some(data) = RowData::from_int(data) else {
        return Result::InvalidValue;
    };

    match data {
        RowData::Invalid => Result::InvalidValue,
        _ => get_typed(row_, data, out),
    }
}

/// C: ghostty_row_get_multi
///
/// # Safety
/// `keys` and `values` must be NULL or valid for `count` elements, each
/// value pointing to storage of its key's output type.
#[export_name = "ghostty_row_get_multi"]
pub unsafe extern "C" fn get_multi(
    row_: CRow,
    count: usize,
    keys: *const RowData,
    values: *const *mut c_void,
    out_written: Option<&mut usize>,
) -> Result {
    if keys.is_null() || values.is_null() {
        return Result::InvalidValue;
    }
    // Keys are read as raw integers: a C caller may pass any value.
    let k = keys as *const c_int;

    for i in 0..count {
        let result = get(row_, *k.add(i), *values.add(i));
        if result != Result::Success {
            if let Some(w) = out_written {
                *w = i;
            }
            return result;
        }
    }
    if let Some(w) = out_written {
        *w = count;
    }
    Result::Success
}

unsafe fn get_typed(row_: CRow, data: RowData, out: *mut c_void) -> Result {
    let row = Row::from_bits(row_);
    match data {
        RowData::Invalid => return Result::InvalidValue,
        RowData::Wrap => *(out as *mut bool) = row.wrap(),
        RowData::WrapContinuation => *(out as *mut bool) = row.wrap_continuation(),
        RowData::Grapheme => *(out as *mut bool) = row.grapheme(),
        RowData::Styled => *(out as *mut bool) = row.styled(),
        RowData::Hyperlink => *(out as *mut bool) = row.hyperlink(),
        RowData::SemanticPrompt => {
            *(out as *mut SemanticPrompt) = SemanticPrompt::from_int(row.semantic_prompt() as c_int).unwrap()
        }
        RowData::KittyVirtualPlaceholder => *(out as *mut bool) = row.kitty_virtual_placeholder(),
        RowData::Dirty => *(out as *mut bool) = row.dirty(),
    }

    Result::Success
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::page::SemanticPrompt as PageSemanticPrompt;

    fn out<T>(v: &mut T) -> *mut c_void {
        v as *mut T as *mut c_void
    }

    #[test]
    fn get_wrap() {
        let mut zig_row = Row::from_bits(0);
        zig_row.set_wrap(true);
        let row: CRow = zig_row.to_bits();
        let mut wrap = false;
        assert_eq!(Result::Success, unsafe { get(row, RowData::Wrap as c_int, out(&mut wrap)) });
        assert!(wrap);
    }

    #[test]
    fn get_semantic_prompt() {
        let mut zig_row = Row::from_bits(0);
        zig_row.set_semantic_prompt(PageSemanticPrompt::Prompt);
        let row: CRow = zig_row.to_bits();
        let mut sp = SemanticPrompt::None;
        assert_eq!(Result::Success, unsafe { get(row, RowData::SemanticPrompt as c_int, out(&mut sp)) });
        assert_eq!(SemanticPrompt::Prompt, sp);
    }

    #[test]
    fn get_dirty() {
        let mut zig_row = Row::from_bits(0);
        zig_row.set_dirty(true);
        let row: CRow = zig_row.to_bits();
        let mut dirty = false;
        assert_eq!(Result::Success, unsafe { get(row, RowData::Dirty as c_int, out(&mut dirty)) });
        assert!(dirty);
    }

    #[test]
    fn get_multi_success() {
        let mut zig_row = Row::from_bits(0);
        zig_row.set_wrap(true);
        zig_row.set_dirty(true);
        let row_val: CRow = zig_row.to_bits();

        let mut wrap = false;
        let mut dirty = false;
        let mut written: usize = 0;

        let keys = [RowData::Wrap, RowData::Dirty];
        let values = [out(&mut wrap), out(&mut dirty)];
        assert_eq!(Result::Success, unsafe {
            get_multi(row_val, keys.len(), keys.as_ptr(), values.as_ptr(), Some(&mut written))
        });
        assert_eq!(keys.len(), written);
        assert!(wrap);
        assert!(dirty);
    }

    #[test]
    fn get_multi_error_sets_out_written() {
        let mut zig_row = Row::from_bits(0);
        zig_row.set_wrap(true);
        let row_val: CRow = zig_row.to_bits();

        let mut wrap = false;
        let mut written: usize = 99;

        let keys = [RowData::Wrap, RowData::Invalid];
        let values = [out(&mut wrap), out(&mut wrap)];
        assert_eq!(Result::InvalidValue, unsafe {
            get_multi(row_val, keys.len(), keys.as_ptr(), values.as_ptr(), Some(&mut written))
        });
        assert_eq!(1, written);
        assert!(wrap);
    }

    #[test]
    fn get_multi_null_keys_returns_invalid_value() {
        let row_val: CRow = 0;
        let mut wrap = false;
        let values = [out(&mut wrap)];
        assert_eq!(Result::InvalidValue, unsafe {
            get_multi(row_val, 1, std::ptr::null(), values.as_ptr(), None)
        });
    }
}
