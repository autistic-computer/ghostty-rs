//! Port of `src/terminal/c/cell.zig`.
//!
//! STATUS: ported

use std::ffi::{c_int, c_void};

use super::result::Result;
use crate::terminal::color::RgbC;
use crate::terminal::page::Cell;

/// C: GhosttyCell
pub type CCell = u64;

/// C: GhosttyCellsView
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CellsView {
    pub ptr: *const CCell,
    pub len: usize,
}

crate::c_api_enum! {
    /// C: GhosttyCellContentTag
    pub enum ContentTag {
        Codepoint = 0,
        CodepointGrapheme = 1,
        BgColorPalette = 2,
        BgColorRgb = 3,
    }
}

crate::c_api_enum! {
    /// C: GhosttyCellWide
    pub enum Wide {
        Narrow = 0,
        Wide = 1,
        SpacerTail = 2,
        SpacerHead = 3,
    }
}

crate::c_api_enum! {
    /// C: GhosttyCellSemanticContent
    pub enum SemanticContent {
        Output = 0,
        Input = 1,
        Prompt = 2,
    }
}

crate::c_api_enum! {
    /// C: GhosttyCellData
    ///
    /// Output type expected for querying the data of the given kind (Zig:
    /// `CellData.OutType`) is documented on each key.
    pub enum CellData {
        Invalid = 0,

        /// The codepoint of the cell (0 if empty or bg-color-only).
        /// Output type: uint32_t * (stored as u21, zero-extended)
        Codepoint = 1,

        /// The content tag describing what kind of content is in the cell.
        /// Output type: GhosttyCellContentTag *
        ContentTag = 2,

        /// The wide property of the cell.
        /// Output type: GhosttyCellWide *
        Wide = 3,

        /// Whether the cell has text to render.
        /// Output type: bool *
        HasText = 4,

        /// Whether the cell has styling (non-default style).
        /// Output type: bool *
        HasStyling = 5,

        /// The style ID for the cell (for use with style lookups).
        /// Output type: uint16_t *
        StyleId = 6,

        /// Whether the cell has a hyperlink.
        /// Output type: bool *
        HasHyperlink = 7,

        /// Whether the cell is protected.
        /// Output type: bool *
        Protected = 8,

        /// The semantic content type of the cell (from OSC 133).
        /// Output type: GhosttyCellSemanticContent *
        SemanticContent = 9,

        /// The palette index for the cell's background color.
        /// Only valid when content_tag is bg_color_palette.
        /// Output type: GhosttyColorPaletteIndex *
        ColorPalette = 10,

        /// The RGB value for the cell's background color.
        /// Only valid when content_tag is bg_color_rgb.
        /// Output type: GhosttyColorRgb *
        ColorRgb = 11,
    }
}

/// C: ghostty_cell_get
///
/// # Safety
/// `out` must point to storage of the output type for `data`.
#[export_name = "ghostty_cell_get"]
pub unsafe extern "C" fn get(cell_: CCell, data: c_int, out: *mut c_void) -> Result {
    let Some(data) = CellData::from_int(data) else {
        return Result::InvalidValue;
    };

    match data {
        CellData::Invalid => Result::InvalidValue,
        _ => get_typed(cell_, data, out),
    }
}

/// C: ghostty_cell_get_multi
///
/// # Safety
/// `keys` and `values` must be NULL or valid for `count` elements, each
/// value pointing to storage of its key's output type.
#[export_name = "ghostty_cell_get_multi"]
pub unsafe extern "C" fn get_multi(
    cell_: CCell,
    count: usize,
    keys: *const CellData,
    values: *const *mut c_void,
    out_written: Option<&mut usize>,
) -> Result {
    if keys.is_null() || values.is_null() {
        return Result::InvalidValue;
    }
    // Keys are read as raw integers: a C caller may pass any value.
    let k = keys as *const c_int;

    for i in 0..count {
        let result = get(cell_, *k.add(i), *values.add(i));
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

unsafe fn get_typed(cell_: CCell, data: CellData, out: *mut c_void) -> Result {
    let cell = Cell::from_bits(cell_);
    match data {
        CellData::Invalid => return Result::InvalidValue,
        CellData::Codepoint => *(out as *mut u32) = cell.codepoint(),
        CellData::ContentTag => {
            *(out as *mut ContentTag) = ContentTag::from_int(cell.content_tag() as c_int).unwrap()
        }
        CellData::Wide => *(out as *mut Wide) = Wide::from_int(cell.wide() as c_int).unwrap(),
        CellData::HasText => *(out as *mut bool) = cell.has_text(),
        CellData::HasStyling => *(out as *mut bool) = cell.has_styling(),
        CellData::StyleId => *(out as *mut u16) = cell.style_id(),
        CellData::HasHyperlink => *(out as *mut bool) = cell.hyperlink(),
        CellData::Protected => *(out as *mut bool) = cell.protected(),
        CellData::SemanticContent => {
            *(out as *mut SemanticContent) =
                SemanticContent::from_int(cell.semantic_content() as c_int).unwrap()
        }
        CellData::ColorPalette => *(out as *mut u8) = cell.content_color_palette(),
        CellData::ColorRgb => {
            let rgb = cell.content_color_rgb();
            *(out as *mut RgbC) = RgbC {
                r: rgb.r,
                g: rgb.g,
                b: rgb.b,
            };
        }
    }

    Result::Success
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::page::Wide as PageWide;

    fn out<T>(v: &mut T) -> *mut c_void {
        v as *mut T as *mut c_void
    }

    #[test]
    fn get_codepoint() {
        let cell: CCell = Cell::init('A' as u32).to_bits();
        let mut cp: u32 = 0;
        assert_eq!(Result::Success, unsafe { get(cell, CellData::Codepoint as c_int, out(&mut cp)) });
        assert_eq!('A' as u32, cp);
    }

    #[test]
    fn get_has_text() {
        let cell: CCell = Cell::init('A' as u32).to_bits();
        let mut has = false;
        assert_eq!(Result::Success, unsafe { get(cell, CellData::HasText as c_int, out(&mut has)) });
        assert!(has);
    }

    #[test]
    fn get_empty_cell() {
        let cell: CCell = Cell::init(0).to_bits();
        let mut has = true;
        assert_eq!(Result::Success, unsafe { get(cell, CellData::HasText as c_int, out(&mut has)) });
        assert!(!has);
    }

    #[test]
    fn get_wide() {
        let mut zig_cell = Cell::init('A' as u32);
        zig_cell.set_wide(PageWide::Wide);
        let cell: CCell = zig_cell.to_bits();
        let mut w = Wide::Narrow;
        assert_eq!(Result::Success, unsafe { get(cell, CellData::Wide as c_int, out(&mut w)) });
        assert_eq!(Wide::Wide, w);
    }

    #[test]
    fn get_multi_success() {
        let cell: CCell = Cell::init('B' as u32).to_bits();
        let mut cp: u32 = 0;
        let mut has_text = false;
        let mut written: usize = 0;

        let keys = [CellData::Codepoint, CellData::HasText];
        let values = [out(&mut cp), out(&mut has_text)];
        assert_eq!(Result::Success, unsafe {
            get_multi(cell, keys.len(), keys.as_ptr(), values.as_ptr(), Some(&mut written))
        });
        assert_eq!(keys.len(), written);
        assert_eq!('B' as u32, cp);
        assert!(has_text);
    }

    #[test]
    fn get_multi_error_sets_out_written() {
        let cell: CCell = Cell::init('C' as u32).to_bits();
        let mut cp: u32 = 0;
        let mut written: usize = 99;

        let keys = [CellData::Codepoint, CellData::Invalid];
        let values = [out(&mut cp), out(&mut cp)];
        assert_eq!(Result::InvalidValue, unsafe {
            get_multi(cell, keys.len(), keys.as_ptr(), values.as_ptr(), Some(&mut written))
        });
        assert_eq!(1, written);
        assert_eq!('C' as u32, cp);
    }

    #[test]
    fn get_multi_null_keys_returns_invalid_value() {
        let cell: CCell = Cell::init('A' as u32).to_bits();
        let mut cp: u32 = 0;
        let values = [out(&mut cp)];
        assert_eq!(Result::InvalidValue, unsafe {
            get_multi(cell, 1, std::ptr::null(), values.as_ptr(), None)
        });
    }

    #[test]
    fn get_multi_null_values_returns_invalid_value() {
        let cell: CCell = Cell::init('A' as u32).to_bits();
        let keys = [CellData::Codepoint];
        assert_eq!(Result::InvalidValue, unsafe {
            get_multi(cell, 1, keys.as_ptr(), std::ptr::null(), None)
        });
    }
}
