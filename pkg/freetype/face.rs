//! Port of `pkg/freetype/face.zig`.
//!
//! STATUS: ported

use std::os::raw::{c_int, c_ulong};

use super::c;
use super::errors::{int_to_error, Error};
use super::tag::Tag;

/// A FreeType face handle. Like the Zig struct this is a thin, copyable
/// wrapper around `FT_Face`; FreeType reference counts faces (see
/// [`Face::r#ref`]) and each reference is released with [`Face::deinit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Face {
    pub handle: c::FT_Face,
}

impl Face {
    /// # Safety
    /// This releases one reference to the face; if it was the last one, the
    /// face (and every copy of this handle) must not be used afterwards.
    pub unsafe fn deinit(self) {
        c::FT_Done_Face(self.handle);
    }

    /// Increment the counter of the face.
    pub fn r#ref(self) {
        // SAFETY: valid face handle.
        unsafe {
            c::FT_Reference_Face(self.handle);
        }
    }

    /// A macro that returns true whenever a face object contains some
    /// embedded bitmaps. See the available_sizes field of the FT_FaceRec structure.
    pub fn has_fixed_sizes(self) -> bool {
        // SAFETY: valid face handle.
        unsafe { c::FT_HAS_FIXED_SIZES(self.handle) }
    }

    /// A macro that returns true whenever a face object contains tables for
    /// color glyphs.
    pub fn has_color(self) -> bool {
        // SAFETY: valid face handle.
        unsafe { c::FT_HAS_COLOR(self.handle) }
    }

    /// A macro that returns true whenever a face object contains an ‘sbix’
    /// OpenType table and outline glyphs.
    pub fn has_sbix(self) -> bool {
        // SAFETY: valid face handle.
        unsafe { c::FT_HAS_SBIX(self.handle) }
    }

    /// A macro that returns true whenever a face object contains some
    /// multiple masters.
    pub fn has_multiple_masters(self) -> bool {
        // SAFETY: valid face handle.
        unsafe { c::FT_HAS_MULTIPLE_MASTERS(self.handle) }
    }

    /// A macro that returns true whenever a face object contains a scalable
    /// font face (true for TrueType, Type 1, Type 42, CID, OpenType/CFF,
    /// and PFR font formats).
    pub fn is_scalable(self) -> bool {
        // SAFETY: valid face handle.
        unsafe { c::FT_IS_SCALABLE(self.handle) }
    }

    /// Select a given charmap by its encoding tag (as listed in freetype.h).
    pub fn select_charmap(self, encoding: Encoding) -> Result<(), Error> {
        // SAFETY: valid face handle.
        int_to_error(unsafe { c::FT_Select_Charmap(self.handle, encoding as c::FT_Encoding) })
    }

    /// Call FT_Request_Size to request the nominal size (in points).
    pub fn set_char_size(
        self,
        char_width: i32,
        char_height: i32,
        horz_resolution: u16,
        vert_resolution: u16,
    ) -> Result<(), Error> {
        // SAFETY: valid face handle.
        int_to_error(unsafe {
            c::FT_Set_Char_Size(
                self.handle,
                char_width as c::FT_F26Dot6,
                char_height as c::FT_F26Dot6,
                horz_resolution as c::FT_UInt,
                vert_resolution as c::FT_UInt,
            )
        })
    }

    /// Select a bitmap strike. To be more precise, this function sets the
    /// scaling factors of the active FT_Size object in a face so that bitmaps
    /// from this particular strike are taken by FT_Load_Glyph and friends.
    pub fn select_size(self, idx: i32) -> Result<(), Error> {
        // SAFETY: valid face handle.
        int_to_error(unsafe { c::FT_Select_Size(self.handle, idx) })
    }

    /// Return the glyph index of a given character code. This function uses
    /// the currently selected charmap to do the mapping.
    pub fn get_char_index(self, char: u32) -> Option<u32> {
        // SAFETY: valid face handle.
        let i = unsafe { c::FT_Get_Char_Index(self.handle, char as c::FT_ULong) };
        if i == 0 {
            None
        } else {
            Some(i)
        }
    }

    /// Load a glyph into the glyph slot of a face object.
    pub fn load_glyph(self, glyph_index: u32, load_flags: LoadFlags) -> Result<(), Error> {
        // SAFETY: valid face handle.
        int_to_error(unsafe { c::FT_Load_Glyph(self.handle, glyph_index, load_flags.to_bits()) })
    }

    /// Convert a given glyph image to a bitmap.
    pub fn render_glyph(self, render_mode: RenderMode) -> Result<(), Error> {
        // SAFETY: valid face handle; the glyph slot is owned by the face.
        int_to_error(unsafe {
            c::FT_Render_Glyph((*self.handle).glyph, render_mode as c::FT_Render_Mode)
        })
    }

    /// Return a pointer to a given SFNT table stored within a face.
    ///
    /// The Zig version takes the tag as a comptime parameter and derives the
    /// return type from it (`SfntTag.DataType`); in Rust the table type
    /// selects the tag via [`SfntTable`].
    pub fn get_sfnt_table<T: SfntTable>(&self) -> Option<&T> {
        // SAFETY: valid face handle; FreeType returns either null or a
        // pointer to a table of type `T` owned by the face.
        unsafe {
            let ptr = c::FT_Get_Sfnt_Table(self.handle, T::TAG as c::FT_Sfnt_Tag);
            (ptr as *const T).as_ref()
        }
    }

    /// Retrieve the number of name strings in the SFNT ‘name’ table.
    pub fn get_sfnt_name_count(self) -> usize {
        // SAFETY: valid face handle.
        unsafe { c::FT_Get_Sfnt_Name_Count(self.handle) as usize }
    }

    /// Retrieve a string of the SFNT ‘name’ table for a given index.
    pub fn get_sfnt_name(self, i: usize) -> Result<c::FT_SfntName, Error> {
        // SAFETY: valid face handle and out pointer; FT_SfntName is POD.
        unsafe {
            let mut name: c::FT_SfntName = std::mem::zeroed();
            int_to_error(c::FT_Get_Sfnt_Name(self.handle, i as c::FT_UInt, &mut name))?;
            Ok(name)
        }
    }

    /// Load any SFNT font table into client memory.
    pub fn load_sfnt_table(self, tag: Tag) -> Result<Option<Vec<u8>>, Error> {
        let tag_c = tag.to_int() as c_ulong;

        // Get the length of the table in bytes
        let mut len: c_ulong = 0;
        // SAFETY: valid face handle; null buffer queries the length.
        int_to_error(unsafe {
            c::FT_Load_Sfnt_Table(self.handle, tag_c, 0, std::ptr::null_mut(), &mut len)
        })?;

        // If our length is zero we don't have a table.
        if len == 0 {
            return Ok(None);
        }

        // Allocate a buffer to hold the table and load it
        let mut buf = vec![0u8; len as usize];
        // SAFETY: buf has room for `len` bytes.
        int_to_error(unsafe {
            c::FT_Load_Sfnt_Table(self.handle, tag_c, 0, buf.as_mut_ptr(), &mut len)
        })?;

        Ok(Some(buf))
    }

    /// Check whether a given SFNT table is available in a face.
    pub fn has_sfnt_table(self, tag: Tag) -> bool {
        let tag_c = tag.to_int() as c_ulong;
        let mut len: c_ulong = 0;
        // SAFETY: valid face handle; null buffer queries the length.
        let res =
            unsafe { c::FT_Load_Sfnt_Table(self.handle, tag_c, 0, std::ptr::null_mut(), &mut len) };
        if int_to_error(res).is_err() {
            return false;
        }
        len != 0
    }

    /// Retrieve the font variation descriptor for a font.
    ///
    /// The result must be released with
    /// [`Library::done_mm_var`](super::library::Library::done_mm_var).
    pub fn get_mm_var(self) -> Result<*mut c::FT_MM_Var, Error> {
        let mut result: *mut c::FT_MM_Var = std::ptr::null_mut();
        // SAFETY: valid face handle and out pointer.
        int_to_error(unsafe { c::FT_Get_MM_Var(self.handle, &mut result) })?;
        Ok(result)
    }

    /// Get the design coordinates of the currently selected interpolated font.
    pub fn get_var_design_coordinates(self, coords: &mut [c::FT_Fixed]) -> Result<(), Error> {
        // SAFETY: valid face handle; coords is valid for coords.len() items.
        int_to_error(unsafe {
            c::FT_Get_Var_Design_Coordinates(
                self.handle,
                coords.len() as c::FT_UInt,
                coords.as_mut_ptr(),
            )
        })
    }

    /// Choose an interpolated font design through design coordinates.
    pub fn set_var_design_coordinates(self, coords: &[c::FT_Fixed]) -> Result<(), Error> {
        // SAFETY: valid face handle; FreeType only reads the coordinates.
        int_to_error(unsafe {
            c::FT_Set_Var_Design_Coordinates(
                self.handle,
                coords.len() as c::FT_UInt,
                coords.as_ptr() as *mut c::FT_Fixed,
            )
        })
    }

    /// Set the transformation that is applied to glyph images when they are
    /// loaded into a glyph slot through FT_Load_Glyph.
    pub fn set_transform(self, matrix: Option<&c::FT_Matrix>, delta: Option<&c::FT_Vector>) {
        // SAFETY: valid face handle; FreeType copies the matrix and vector.
        unsafe {
            c::FT_Set_Transform(
                self.handle,
                matrix.map_or(std::ptr::null_mut(), |m| m as *const _ as *mut _),
                delta.map_or(std::ptr::null_mut(), |d| d as *const _ as *mut _),
            );
        }
    }
}

/// An enumeration to specify indices of SFNT tables loaded and parsed by
/// FreeType during initialization of an SFNT font. Used in the
/// FT_Get_Sfnt_Table API function.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SfntTag {
    Head = c::FT_SFNT_HEAD,
    Maxp = c::FT_SFNT_MAXP,
    Os2 = c::FT_SFNT_OS2,
    Hhea = c::FT_SFNT_HHEA,
    Vhea = c::FT_SFNT_VHEA,
    Post = c::FT_SFNT_POST,
    Pclt = c::FT_SFNT_PCLT,
}

/// The data type for a given sfnt tag (Zig: `SfntTag.DataType`).
pub trait SfntTable {
    const TAG: SfntTag;
}

impl SfntTable for c::TT_OS2 {
    const TAG: SfntTag = SfntTag::Os2;
}

impl SfntTable for c::TT_Header {
    const TAG: SfntTag = SfntTag::Head;
}

impl SfntTable for c::TT_Postscript {
    const TAG: SfntTag = SfntTag::Post;
}

impl SfntTable for c::TT_HoriHeader {
    const TAG: SfntTag = SfntTag::Hhea;
}

/// An enumeration to specify character sets supported by charmaps. Used in the
/// FT_Select_Charmap API function.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    None = c::FT_ENCODING_NONE,
    MsSymbol = c::FT_ENCODING_MS_SYMBOL,
    Unicode = c::FT_ENCODING_UNICODE,
    Sjis = c::FT_ENCODING_SJIS,
    Prc = c::FT_ENCODING_PRC,
    Big5 = c::FT_ENCODING_BIG5,
    Wansung = c::FT_ENCODING_WANSUNG,
    Johab = c::FT_ENCODING_JOHAB,
    AdobeStandard = c::FT_ENCODING_ADOBE_STANDARD,
    AdobeExpert = c::FT_ENCODING_ADOBE_EXPERT,
    AdobeCustom = c::FT_ENCODING_ADOBE_CUSTOM,
    AdobeLatin1 = c::FT_ENCODING_ADOBE_LATIN_1,
    OldLatin2 = c::FT_ENCODING_OLD_LATIN_2,
    AppleRoman = c::FT_ENCODING_APPLE_ROMAN,
}

/// https://freetype.org/freetype2/docs/reference/ft2-glyph_retrieval.html#ft_render_mode
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderMode {
    Normal = c::FT_RENDER_MODE_NORMAL,
    Light = c::FT_RENDER_MODE_LIGHT,
    Mono = c::FT_RENDER_MODE_MONO,
    Lcd = c::FT_RENDER_MODE_LCD,
    LcdV = c::FT_RENDER_MODE_LCD_V,
    Sdf = c::FT_RENDER_MODE_SDF,
}

/// A collection of flags for FT_Load_Glyph that indicate
/// what kind of operations to perform during glyph loading.
///
/// Some of these flags are not included in the official FreeType
/// documentation, but are nevertheless present and named in the
/// header, so the names have been copied from there.
///
/// Zig: `packed struct(c_int)`; [`LoadFlags::to_bits`] and
/// [`LoadFlags::from_bits`] are the `@bitCast`s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LoadFlags {
    pub no_scale: bool,
    pub no_hinting: bool,
    pub render: bool,
    pub no_bitmap: bool,
    pub vertical_layout: bool,
    pub force_autohint: bool,
    pub crop_bitmap: bool,
    pub pedantic: bool,
    pub advance_only: bool,
    pub ignore_global_advance_width: bool,
    pub no_recurse: bool,
    pub ignore_transform: bool,
    pub monochrome: bool,
    pub linear_design: bool,
    pub sbits_only: bool,
    pub no_autohint: bool,
    pub target: LoadTarget,
    pub color: bool,
    pub compute_metrics: bool,
    pub bitmap_metrics_only: bool,
    pub svg_only: bool,
    pub no_svg: bool,
}

/// Zig: `LoadFlags.Target` (`enum(u4)`).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LoadTarget {
    #[default]
    Normal = 0,
    Light = 1,
    Mono = 2,
    Lcd = 3,
    LcdV = 4,
}

impl LoadFlags {
    const TARGET_SHIFT: u32 = 16;

    /// `@bitCast(flags)` to the C `FT_LOAD_*` value.
    pub const fn to_bits(self) -> c_int {
        let bools = [
            self.no_scale,
            self.no_hinting,
            self.render,
            self.no_bitmap,
            self.vertical_layout,
            self.force_autohint,
            self.crop_bitmap,
            self.pedantic,
            self.advance_only,
            self.ignore_global_advance_width,
            self.no_recurse,
            self.ignore_transform,
            self.monochrome,
            self.linear_design,
            self.sbits_only,
            self.no_autohint,
        ];
        let mut v: u32 = 0;
        let mut i = 0;
        while i < bools.len() {
            if bools[i] {
                v |= 1 << i;
            }
            i += 1;
        }
        v |= (self.target as u32) << Self::TARGET_SHIFT;
        let hi = [
            self.color,
            self.compute_metrics,
            self.bitmap_metrics_only,
            self.svg_only,
            self.no_svg,
        ];
        let mut j = 0;
        while j < hi.len() {
            if hi[j] {
                v |= 1 << (20 + j);
            }
            j += 1;
        }
        v as c_int
    }

    /// `@bitCast(value)` from a C `FT_LOAD_*` value.
    pub fn from_bits(value: c_int) -> LoadFlags {
        let v = value as u32;
        let bit = |i: u32| v & (1 << i) != 0;
        LoadFlags {
            no_scale: bit(0),
            no_hinting: bit(1),
            render: bit(2),
            no_bitmap: bit(3),
            vertical_layout: bit(4),
            force_autohint: bit(5),
            crop_bitmap: bit(6),
            pedantic: bit(7),
            advance_only: bit(8),
            ignore_global_advance_width: bit(9),
            no_recurse: bit(10),
            ignore_transform: bit(11),
            monochrome: bit(12),
            linear_design: bit(13),
            sbits_only: bit(14),
            no_autohint: bit(15),
            target: match (v >> Self::TARGET_SHIFT) & 0xF {
                0 => LoadTarget::Normal,
                1 => LoadTarget::Light,
                2 => LoadTarget::Mono,
                3 => LoadTarget::Lcd,
                4 => LoadTarget::LcdV,
                // Illegal in Zig (invalid enum value in @bitCast).
                _ => panic!("invalid FT_LOAD_TARGET value"),
            },
            color: bit(20),
            compute_metrics: bit(21),
            bitmap_metrics_only: bit(22),
            svg_only: bit(23),
            no_svg: bit(24),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::library::Library;
    use super::super::test::FONT_REGULAR;
    use super::*;

    #[test]
    fn bitcast() {
        let cval: i32 = c::FT_LOAD_RENDER | c::FT_LOAD_PEDANTIC | c::FT_LOAD_COLOR;
        let flags = LoadFlags::from_bits(cval);
        assert!(!flags.no_hinting);
        assert!(flags.render);
        assert!(flags.pedantic);
        assert!(flags.color);

        // Verify bit alignment (for bit 9)
        let cval2: i32 = c::FT_LOAD_IGNORE_GLOBAL_ADVANCE_WIDTH;
        let flags2 = LoadFlags::from_bits(cval2);
        assert!(flags2.ignore_global_advance_width);
        assert!(!flags2.no_recurse);
    }

    #[test]
    fn all_flags_individually() {
        assert_eq!(c::FT_LOAD_DEFAULT, LoadFlags::default().to_bits());

        type Setter = fn(&mut LoadFlags);
        let pairs: [(c_int, &str, Setter); 21] = [
            (c::FT_LOAD_NO_SCALE, "no_scale", |f| f.no_scale = true),
            (c::FT_LOAD_NO_HINTING, "no_hinting", |f| f.no_hinting = true),
            (c::FT_LOAD_RENDER, "render", |f| f.render = true),
            (c::FT_LOAD_NO_BITMAP, "no_bitmap", |f| f.no_bitmap = true),
            (c::FT_LOAD_VERTICAL_LAYOUT, "vertical_layout", |f| {
                f.vertical_layout = true
            }),
            (c::FT_LOAD_FORCE_AUTOHINT, "force_autohint", |f| {
                f.force_autohint = true
            }),
            (c::FT_LOAD_CROP_BITMAP, "crop_bitmap", |f| {
                f.crop_bitmap = true
            }),
            (c::FT_LOAD_PEDANTIC, "pedantic", |f| f.pedantic = true),
            (c::FT_LOAD_ADVANCE_ONLY, "advance_only", |f| {
                f.advance_only = true
            }),
            (
                c::FT_LOAD_IGNORE_GLOBAL_ADVANCE_WIDTH,
                "ignore_global_advance_width",
                |f| f.ignore_global_advance_width = true,
            ),
            (c::FT_LOAD_NO_RECURSE, "no_recurse", |f| f.no_recurse = true),
            (c::FT_LOAD_IGNORE_TRANSFORM, "ignore_transform", |f| {
                f.ignore_transform = true
            }),
            (c::FT_LOAD_MONOCHROME, "monochrome", |f| f.monochrome = true),
            (c::FT_LOAD_LINEAR_DESIGN, "linear_design", |f| {
                f.linear_design = true
            }),
            (c::FT_LOAD_SBITS_ONLY, "sbits_only", |f| f.sbits_only = true),
            (c::FT_LOAD_NO_AUTOHINT, "no_autohint", |f| {
                f.no_autohint = true
            }),
            (c::FT_LOAD_COLOR, "color", |f| f.color = true),
            (c::FT_LOAD_COMPUTE_METRICS, "compute_metrics", |f| {
                f.compute_metrics = true
            }),
            (c::FT_LOAD_BITMAP_METRICS_ONLY, "bitmap_metrics_only", |f| {
                f.bitmap_metrics_only = true
            }),
            (c::FT_LOAD_SVG_ONLY, "svg_only", |f| f.svg_only = true),
            (c::FT_LOAD_NO_SVG, "no_svg", |f| f.no_svg = true),
        ];
        for (cval, name, set) in pairs {
            let mut flags = LoadFlags::default();
            set(&mut flags);
            assert_eq!(cval, flags.to_bits(), "{name}");
            assert_eq!(flags, LoadFlags::from_bits(cval), "{name}");
        }
    }

    #[test]
    fn all_load_targets() {
        for (cval, target) in [
            (c::FT_LOAD_TARGET_NORMAL, LoadTarget::Normal),
            (c::FT_LOAD_TARGET_LIGHT, LoadTarget::Light),
            (c::FT_LOAD_TARGET_MONO, LoadTarget::Mono),
            (c::FT_LOAD_TARGET_LCD, LoadTarget::Lcd),
            (c::FT_LOAD_TARGET_LCD_V, LoadTarget::LcdV),
        ] {
            let flags = LoadFlags {
                target,
                ..Default::default()
            };
            assert_eq!(cval, flags.to_bits());
        }
    }

    #[test]
    fn loading_memory_font() {
        let font_data = FONT_REGULAR;

        let lib = Library::init().unwrap();
        // SAFETY: font_data is 'static.
        let face = unsafe { lib.init_memory_face(font_data, 0) }.unwrap();

        // Try APIs
        face.select_charmap(Encoding::Unicode).unwrap();
        assert!(!face.has_fixed_sizes());
        face.set_char_size(12, 0, 0, 0).unwrap();

        // Try loading
        let idx = face.get_char_index('A' as u32).unwrap();
        face.load_glyph(idx, LoadFlags::default()).unwrap();

        // Try getting a truetype table
        let os2 = face.get_sfnt_table::<c::TT_OS2>();
        assert!(os2.is_some());

        // SAFETY: not used afterwards.
        unsafe {
            face.deinit();
            lib.deinit();
        }
    }
}
