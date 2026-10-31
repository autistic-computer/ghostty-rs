//! Port of `pkg/harfbuzz/buffer.zig`.
//!
//! STATUS: ported

use std::os::raw::{c_char, c_int, c_uint};

use super::c;
use super::common::{Direction, Language, Script};
use super::errors::Error;

/// Buffers serve a dual role in HarfBuzz; before shaping, they hold the
/// input characters that are passed to hb_shape(), and after shaping they
/// hold the output glyphs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Buffer {
    pub handle: *mut c::hb_buffer_t,
}

impl Buffer {
    /// Creates a new hb_buffer_t with all properties to defaults.
    pub fn create() -> Result<Buffer, Error> {
        // SAFETY: no preconditions.
        let handle = unsafe { c::hb_buffer_create() };
        if handle.is_null() {
            return Err(Error::HarfbuzzFailed);
        }
        Ok(Buffer { handle })
    }

    /// Deallocate the buffer . Decreases the reference count on buffer by one.
    /// If the result is zero, then buffer and all associated resources are
    /// freed. See hb_buffer_reference().
    ///
    /// # Safety
    /// The handle (and its copies) must not be used afterwards unless other
    /// references are still held.
    pub unsafe fn destroy(&mut self) {
        c::hb_buffer_destroy(self.handle);
    }

    /// Resets the buffer to its initial status, as if it was just newly
    /// created with hb_buffer_create().
    pub fn reset(self) {
        // SAFETY: valid buffer handle.
        unsafe { c::hb_buffer_reset(self.handle) }
    }

    /// Returns the number of items in the buffer.
    pub fn get_length(self) -> u32 {
        // SAFETY: valid buffer handle.
        unsafe { c::hb_buffer_get_length(self.handle) }
    }

    /// Sets the type of buffer contents. Buffers are either empty, contain
    /// characters (before shaping), or contain glyphs (the result of shaping).
    pub fn set_content_type(self, ct: ContentType) {
        // SAFETY: valid buffer handle.
        unsafe { c::hb_buffer_set_content_type(self.handle, ct as c::hb_buffer_content_type_t) }
    }

    /// Fetches the type of buffer contents. Buffers are either empty, contain
    /// characters (before shaping), or contain glyphs (the result of shaping).
    pub fn get_content_type(self) -> ContentType {
        // SAFETY: valid buffer handle.
        ContentType::from_raw(unsafe { c::hb_buffer_get_content_type(self.handle) })
    }

    /// Appends a character with the Unicode value of codepoint to buffer,
    /// and gives it the initial cluster value of cluster . Clusters can be
    /// any thing the client wants, they are usually used to refer to the
    /// index of the character in the input text stream and are output in
    /// hb_glyph_info_t.cluster field.
    ///
    /// This function does not check the validity of codepoint, it is up to
    /// the caller to ensure it is a valid Unicode code point.
    pub fn add(self, cp: u32, cluster: u32) {
        // SAFETY: valid buffer handle.
        unsafe { c::hb_buffer_add(self.handle, cp, cluster) }
    }

    /// Appends characters from text array to buffer . The item_offset is the
    /// position of the first character from text that will be appended, and
    /// item_length is the number of character. When shaping part of a larger
    /// text (e.g. a run of text from a paragraph), instead of passing just
    /// the substring corresponding to the run, it is preferable to pass the
    /// whole paragraph and specify the run start and length as item_offset and
    /// item_length , respectively, to give HarfBuzz the full context to be
    /// able, for example, to do cross-run Arabic shaping or properly handle
    /// combining marks at stat of run.
    ///
    /// This function does not check the validity of text , it is up to the
    /// caller to ensure it contains a valid Unicode code points.
    pub fn add_codepoints(self, text: &[u32]) {
        // SAFETY: valid buffer handle; text valid for text.len() items.
        unsafe {
            c::hb_buffer_add_codepoints(
                self.handle,
                text.as_ptr(),
                text.len() as c_int,
                0,
                text.len() as c_int,
            )
        }
    }

    /// See hb_buffer_add_codepoints().
    ///
    /// Replaces invalid UTF-32 characters with the buffer replacement code
    /// point, see hb_buffer_set_replacement_codepoint().
    pub fn add_utf32(self, text: &[u32]) {
        // SAFETY: valid buffer handle; text valid for text.len() items.
        unsafe {
            c::hb_buffer_add_utf32(
                self.handle,
                text.as_ptr(),
                text.len() as c_int,
                0,
                text.len() as c_int,
            )
        }
    }

    /// See hb_buffer_add_codepoints().
    ///
    /// Replaces invalid UTF-16 characters with the buffer replacement code
    /// point, see hb_buffer_set_replacement_codepoint().
    pub fn add_utf16(self, text: &[u16]) {
        // SAFETY: valid buffer handle; text valid for text.len() items.
        unsafe {
            c::hb_buffer_add_utf16(
                self.handle,
                text.as_ptr(),
                text.len() as c_int,
                0,
                text.len() as c_int,
            )
        }
    }

    /// See hb_buffer_add_codepoints().
    ///
    /// Replaces invalid UTF-8 characters with the buffer replacement code
    /// point, see hb_buffer_set_replacement_codepoint().
    pub fn add_utf8(self, text: &[u8]) {
        // SAFETY: valid buffer handle; text valid for text.len() items.
        unsafe {
            c::hb_buffer_add_utf8(
                self.handle,
                text.as_ptr() as *const c_char,
                text.len() as c_int,
                0,
                text.len() as c_int,
            )
        }
    }

    /// Similar to hb_buffer_add_codepoints(), but allows only access to first
    /// 256 Unicode code points that can fit in 8-bit strings.
    pub fn add_latin1(self, text: &[u8]) {
        // SAFETY: valid buffer handle; text valid for text.len() items.
        unsafe {
            c::hb_buffer_add_latin1(
                self.handle,
                text.as_ptr(),
                text.len() as c_int,
                0,
                text.len() as c_int,
            )
        }
    }

    /// Set the text flow direction of the buffer. No shaping can happen
    /// without setting buffer direction, and it controls the visual direction
    /// for the output glyphs; for RTL direction the glyphs will be reversed.
    /// Many layout features depend on the proper setting of the direction,
    /// for example, reversing RTL text before shaping, then shaping with LTR
    /// direction is not the same as keeping the text in logical order and
    /// shaping with RTL direction.
    pub fn set_direction(self, dir: Direction) {
        // SAFETY: valid buffer handle.
        unsafe { c::hb_buffer_set_direction(self.handle, dir as c::hb_direction_t) }
    }

    /// See hb_buffer_set_direction()
    pub fn get_direction(self) -> Direction {
        // SAFETY: valid buffer handle.
        Direction::from_raw(unsafe { c::hb_buffer_get_direction(self.handle) })
    }

    /// Sets the script of buffer to script.
    ///
    /// Script is crucial for choosing the proper shaping behaviour for
    /// scripts that require it (e.g. Arabic) and the which OpenType features
    /// defined in the font to be applied.
    ///
    /// You can pass one of the predefined hb_script_t values, or use
    /// hb_script_from_string() or hb_script_from_iso15924_tag() to get the
    /// corresponding script from an ISO 15924 script tag.
    pub fn set_script(self, script: Script) {
        // SAFETY: valid buffer handle.
        unsafe { c::hb_buffer_set_script(self.handle, script as c::hb_script_t) }
    }

    /// See hb_buffer_set_script()
    ///
    /// Scripts that [`Script`] does not list map to [`Script::Unknown`]
    /// (an invalid enum value in the Zig version).
    pub fn get_script(self) -> Script {
        // SAFETY: valid buffer handle.
        Script::from_raw(unsafe { c::hb_buffer_get_script(self.handle) }).unwrap_or(Script::Unknown)
    }

    /// Sets the language of buffer to language .
    ///
    /// Languages are crucial for selecting which OpenType feature to apply to
    /// the buffer which can result in applying language-specific behaviour.
    /// Languages are orthogonal to the scripts, and though they are related,
    /// they are different concepts and should not be confused with each other.
    ///
    /// Use hb_language_from_string() to convert from BCP 47 language tags to
    /// hb_language_t.
    pub fn set_language(self, language: Language) {
        // SAFETY: valid buffer handle.
        unsafe { c::hb_buffer_set_language(self.handle, language.handle) }
    }

    /// See hb_buffer_set_language()
    pub fn get_language(self) -> Language {
        Language {
            // SAFETY: valid buffer handle.
            handle: unsafe { c::hb_buffer_get_language(self.handle) },
        }
    }

    /// Returns buffer glyph information array. Returned pointer is valid as
    /// long as buffer contents are not modified.
    pub fn get_glyph_infos(&self) -> &[GlyphInfo] {
        let mut length: c_uint = 0;
        // SAFETY: valid buffer handle; GlyphInfo has the layout of
        // hb_glyph_info_t and the array has `length` items.
        unsafe {
            let ptr = c::hb_buffer_get_glyph_infos(self.handle, &mut length);
            if ptr.is_null() || length == 0 {
                return &[];
            }
            std::slice::from_raw_parts(ptr as *const GlyphInfo, length as usize)
        }
    }

    /// Returns buffer glyph position array. Returned pointer is valid as
    /// long as buffer contents are not modified.
    ///
    /// If buffer did not have positions before, the positions will be
    /// initialized to zeros, unless this function is called from within a
    /// buffer message callback (see hb_buffer_set_message_func()), in which
    /// case NULL is returned.
    pub fn get_glyph_positions(&self) -> Option<&[GlyphPosition]> {
        let mut length: c_uint = 0;
        // SAFETY: valid buffer handle; GlyphPosition has the layout of
        // hb_glyph_position_t and the array has `length` items.
        unsafe {
            let ptr = c::hb_buffer_get_glyph_positions(self.handle, &mut length);
            if ptr.is_null() {
                return None;
            }
            if length == 0 {
                return Some(&[]);
            }
            Some(std::slice::from_raw_parts(
                ptr as *const GlyphPosition,
                length as usize,
            ))
        }
    }

    /// Sets unset buffer segment properties based on buffer Unicode contents.
    /// If buffer is not empty, it must have content type
    /// HB_BUFFER_CONTENT_TYPE_UNICODE.
    ///
    /// If buffer script is not set (ie. is HB_SCRIPT_INVALID), it will be set
    /// to the Unicode script of the first character in the buffer that has a
    /// script other than HB_SCRIPT_COMMON, HB_SCRIPT_INHERITED, and
    /// HB_SCRIPT_UNKNOWN.
    ///
    /// Next, if buffer direction is not set (ie. is HB_DIRECTION_INVALID), it
    /// will be set to the natural horizontal direction of the buffer script as
    /// returned by hb_script_get_horizontal_direction(). If
    /// hb_script_get_horizontal_direction() returns HB_DIRECTION_INVALID,
    /// then HB_DIRECTION_LTR is used.
    ///
    /// Finally, if buffer language is not set (ie. is HB_LANGUAGE_INVALID), it
    /// will be set to the process's default language as returned by
    /// hb_language_get_default(). This may change in the future by taking
    /// buffer script into consideration when choosing a language. Note that
    /// hb_language_get_default() is NOT threadsafe the first time it is
    /// called. See documentation for that function for details.
    pub fn guess_segment_properties(self) {
        // SAFETY: valid buffer handle.
        unsafe { c::hb_buffer_guess_segment_properties(self.handle) }
    }

    /// Sets the cluster level of a buffer. The `ClusterLevel` dictates one
    /// aspect of how HarfBuzz will treat non-base characters during shaping.
    pub fn set_cluster_level(self, level: ClusterLevel) {
        // SAFETY: valid buffer handle.
        unsafe {
            c::hb_buffer_set_cluster_level(self.handle, level as c::hb_buffer_cluster_level_t)
        }
    }
}

/// The type of hb_buffer_t contents.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentType {
    /// Initial value for new buffer.
    Invalid = c::HB_BUFFER_CONTENT_TYPE_INVALID,

    /// The buffer contains input characters (before shaping).
    Unicode = c::HB_BUFFER_CONTENT_TYPE_UNICODE,

    /// The buffer contains output glyphs (after shaping).
    Glyphs = c::HB_BUFFER_CONTENT_TYPE_GLYPHS,
}

impl ContentType {
    /// `@enumFromInt` for a raw `hb_buffer_content_type_t`.
    pub fn from_raw(v: c::hb_buffer_content_type_t) -> ContentType {
        match v {
            c::HB_BUFFER_CONTENT_TYPE_UNICODE => ContentType::Unicode,
            c::HB_BUFFER_CONTENT_TYPE_GLYPHS => ContentType::Glyphs,
            _ => ContentType::Invalid,
        }
    }
}

/// Data type for holding HarfBuzz's clustering behavior options. The cluster
/// level dictates one aspect of how HarfBuzz will treat non-base characters
/// during shaping.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClusterLevel {
    /// In `monotone_graphemes`, non-base characters are merged into the
    /// cluster of the base character that precedes them. There is also cluster
    /// merging every time the clusters will otherwise become non-monotone.
    /// This is the default cluster level.
    MonotoneGraphemes = c::HB_BUFFER_CLUSTER_LEVEL_MONOTONE_GRAPHEMES,

    /// In `monotone_characters`, non-base characters are initially assigned
    /// their own cluster values, which are not merged into preceding base
    /// clusters. This allows HarfBuzz to perform additional operations like
    /// reorder sequences of adjacent marks. The output is still monotone, but
    /// the cluster values are more granular.
    MonotoneCharacters = c::HB_BUFFER_CLUSTER_LEVEL_MONOTONE_CHARACTERS,

    /// In `characters`, non-base characters are assigned their own cluster
    /// values, which are not merged into preceding base clusters. Moreover,
    /// the cluster values are not merged into monotone order. This is the most
    /// granular cluster level, and it is useful for clients that need to know
    /// the exact cluster values of each character, but is harder to use for
    /// clients, since clusters might appear in any order.
    Characters = c::HB_BUFFER_CLUSTER_LEVEL_CHARACTERS,

    /// In `graphemes`, non-base characters are merged into the cluster of the
    /// base character that precedes them. This is similar to the Unicode
    /// Grapheme Cluster algorithm, but it is not exactly the same. The output
    /// is not forced to be monotone. This is useful for clients that want to
    /// use HarfBuzz as a cheap implementation of the Unicode Grapheme Cluster
    /// algorithm.
    Graphemes = c::HB_BUFFER_CLUSTER_LEVEL_GRAPHEMES,
}

/// The hb_glyph_info_t is the structure that holds information about the
/// glyphs and their relation to input text.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GlyphInfo {
    /// either a Unicode code point (before shaping) or a glyph index (after shaping).
    pub codepoint: u32,
    pub _mask: u32,

    /// the index of the character in the original text that corresponds to
    /// this hb_glyph_info_t, or whatever the client passes to hb_buffer_add().
    /// More than one hb_glyph_info_t can have the same cluster value, if they
    /// resulted from the same character (e.g. one to many glyph substitution),
    /// and when more than one character gets merged in the same glyph (e.g.
    /// many to one glyph substitution) the hb_glyph_info_t will have the
    /// smallest cluster value of them. By default some characters are merged
    /// into the same cluster (e.g. combining marks have the same cluster as
    /// their bases) even if they are separate glyphs, hb_buffer_set_cluster_level()
    /// allow selecting more fine-grained cluster handling.
    pub cluster: u32,
    pub _var1: u32,
    pub _var2: u32,
}

const _: () =
    assert!(std::mem::size_of::<GlyphInfo>() == std::mem::size_of::<c::hb_glyph_info_t>());

/// The hb_glyph_position_t is the structure that holds the positions of the
/// glyph in both horizontal and vertical directions. All positions in
/// hb_glyph_position_t are relative to the current point.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GlyphPosition {
    /// how much the line advances after drawing this glyph when setting text
    /// in horizontal direction.
    pub x_advance: i32,

    /// how much the line advances after drawing this glyph when setting text
    /// in vertical direction.
    pub y_advance: i32,

    /// how much the glyph moves on the X-axis before drawing it, this should
    /// not affect how much the line advances.
    pub x_offset: i32,

    /// how much the glyph moves on the Y-axis before drawing it, this should
    /// not affect how much the line advances.
    pub y_offset: i32,

    pub _var: u32,
}

const _: () =
    assert!(std::mem::size_of::<GlyphPosition>() == std::mem::size_of::<c::hb_glyph_position_t>());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create() {
        let mut buffer = Buffer::create().unwrap();
        buffer.reset();

        // Content type
        buffer.set_content_type(ContentType::Unicode);
        assert_eq!(ContentType::Unicode, buffer.get_content_type());

        // Try add functions
        buffer.add('🥹' as u32, 27);
        let utf32 = ['A' as u32, 'B' as u32, 'C' as u32];
        let utf16 = [b'A' as u16, b'B' as u16, b'C' as u16];
        let utf8 = [b'A', b'B', b'C'];
        buffer.add_codepoints(&utf32);
        buffer.add_utf32(&utf32);
        buffer.add_utf16(&utf16);
        buffer.add_utf8(&utf8);
        buffer.add_latin1(&utf8);

        // Guess properties first
        buffer.guess_segment_properties();

        // Try to set properties
        buffer.set_direction(Direction::Ltr);
        assert_eq!(Direction::Ltr, buffer.get_direction());

        buffer.set_script(Script::Arabic);
        assert_eq!(Script::Arabic, buffer.get_script());

        buffer.set_language(Language::from_string(b"en"));

        // SAFETY: not used afterwards.
        unsafe { buffer.destroy() };
    }
}
