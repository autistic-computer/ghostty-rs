//! Port of `src/font/opentype/svg.zig`.
//!
//! STATUS: ported

use std::cmp::Ordering;
use std::fmt;

use std::io::Cursor;

use super::sfnt::ReadError;
use crate::lib::compat::reader::{self as compat_reader, Endian};

/// Errors that can be returned from [`Svg::init`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SvgError {
    SvgVersionNotSupported,
    EndOfStream,
}

impl From<ReadError> for SvgError {
    fn from(e: ReadError) -> Self {
        match e {
            ReadError::EndOfStream => SvgError::EndOfStream,
        }
    }
}

impl From<std::io::Error> for SvgError {
    fn from(e: std::io::Error) -> Self {
        ReadError::from(e).into()
    }
}

impl fmt::Display for SvgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SvgError::SvgVersionNotSupported => f.write_str("SVGVersionNotSupported"),
            SvgError::EndOfStream => f.write_str("EndOfStream"),
        }
    }
}

impl std::error::Error for SvgError {}

/// SVG glyphs description table.
///
/// This struct is focused purely on the operations we need for Ghostty,
/// namely to be able to look up whether an glyph ID is present in the SVG
/// table or not. This struct isn't meant to be a general purpose SVG table
/// reader.
///
/// References:
/// - https://www.w3.org/2013/10/SVG_in_OpenType/#thesvg
/// - https://learn.microsoft.com/en-us/typography/opentype/spec/svg
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Svg<'a> {
    /// The start and end glyph IDs (inclusive) that are present in the
    /// table. This is used to very quickly include/exclude a glyph from
    /// the table.
    pub start_glyph_id: u16,
    pub end_glyph_id: u16,

    /// All records in the table.
    pub records: &'a [[u8; 12]],
}

impl<'a> Svg<'a> {
    pub fn init(data: &'a [u8]) -> Result<Svg<'a>, SvgError> {
        let mut reader = Cursor::new(data);

        // Version
        if compat_reader::reader_int::<_, u16>(&mut reader, Endian::Big)? != 0 {
            return Err(SvgError::SvgVersionNotSupported);
        }

        // Offset
        let offset = compat_reader::reader_int::<_, u32>(&mut reader, Endian::Big)? as usize;

        // Seek to the offset to get our document list
        if offset > data.len() {
            return Err(SvgError::EndOfStream);
        }
        reader.set_position(offset as u64);

        // Get our document records along with the start/end glyph range.
        let len = compat_reader::reader_int::<_, u16>(&mut reader, Endian::Big)? as usize;

        // Port note: the Zig code reinterprets the rest of the data as
        // records without a bounds check; here the record array (and the
        // first record that is always read) must fit in the data.
        let seek = reader.position() as usize;
        let record_bytes = data
            .get(seek..seek + len.max(1) * 12)
            .ok_or(SvgError::EndOfStream)?;
        let (records, _) = record_bytes.as_chunks::<12>();
        let records = &records[..len];

        let start_range = glyph_range(&records[0])?;
        let end_range = if len == 1 {
            start_range
        } else {
            glyph_range(&records[len - 1])?
        };

        Ok(Svg {
            start_glyph_id: start_range.0,
            end_glyph_id: end_range.1,
            records,
        })
    }

    pub fn has_glyph(&self, glyph_id: u16) -> bool {
        // Fast path: outside the table range
        if glyph_id < self.start_glyph_id || glyph_id > self.end_glyph_id {
            return false;
        }

        // Fast path, matches the start/end glyph IDs
        if glyph_id == self.start_glyph_id || glyph_id == self.end_glyph_id {
            return true;
        }

        // Slow path: binary search our records
        self.records
            .binary_search_by(|record| compare_glyph_id(glyph_id, record).reverse())
            .is_ok()
    }
}

fn compare_glyph_id(glyph_id: u16, record: &[u8; 12]) -> Ordering {
    let Ok((start, end)) = glyph_range(record) else {
        return Ordering::Less;
    };
    if glyph_id < start {
        Ordering::Less
    } else if glyph_id > end {
        Ordering::Greater
    } else {
        Ordering::Equal
    }
}

fn glyph_range(record: &[u8]) -> Result<(u16, u16), ReadError> {
    let mut reader = Cursor::new(record);
    Ok((
        compat_reader::reader_int(&mut reader, Endian::Big)?,
        compat_reader::reader_int(&mut reader, Endian::Big)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svg() {
        let test_font = crate::font::embedded::JULIA_MONO;

        let lib = crate::font::Library::init().unwrap();
        let face = crate::font::Face::init(
            lib.clone(),
            test_font,
            crate::font::face::Options::new(crate::font::face::DesiredSize::new(12.0)),
        )
        .unwrap();

        #[cfg(not(target_os = "macos"))]
        let table = face.copy_table(b"SVG ").unwrap().unwrap();
        #[cfg(target_os = "macos")]
        let table = face.copy_table(b"SVG ").unwrap();

        let svg = Svg::init(&table).unwrap();
        assert_eq!(11482, svg.start_glyph_id);
        assert_eq!(11482, svg.end_glyph_id);
        assert!(svg.has_glyph(11482));
    }

    #[test]
    fn svg_document_list_offset_past_end() {
        let table: [u8; 6] = [
            0, 0, // Version
            0, 0, 0, 7, // Document list offset, one byte past end
        ];

        assert_eq!(Err(SvgError::EndOfStream), Svg::init(&table));
    }
}
