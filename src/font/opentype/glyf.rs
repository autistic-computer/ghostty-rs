//! Port of `src/font/opentype/glyf.zig`.
//!
//! STATUS: ported

use std::fmt;

use std::io::Cursor;

use super::sfnt::{be_struct, Int16, ReadError, Uint16};
use crate::lib::compat::reader::{self as compat_reader, Endian};

/// Glyph Data Table
///
/// This takes a little bit of a different form than other tables that we
/// have parsers for. Due to the fact that this table contains arrays of
/// arbitrary length, we store a pointer (slice) to the underlying data,
/// and then have functions for getting and interpreting specific parts.
///
/// References:
/// - https://learn.microsoft.com/en-us/typography/opentype/spec/glyf
///
/// Field names are in camelCase in the spec (and in the Zig source);
/// they are converted to snake_case here.
#[derive(Debug, Clone, Copy)]
pub struct Glyf<'a> {
    pub data: &'a [u8],
}

/// A decoded glyph outline.
///
/// The `contours` slice is the list of end point indices and
/// `points` owns all the points. Glyf guarantees that contour
/// points are sequential so we can just store the end and calculate
/// the points that way. Use the helpers to make it ergonomic.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outline {
    /// List of contour end points. Calculate the full list of
    /// points using points[prev...this+1]
    pub contours: Vec<Uint16>,

    /// The backing storage of all points in the entry.
    pub points: Vec<Point>,
}

/// A single decoded point in a simple glyph contour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Point {
    pub x: i32,
    pub y: i32,
    pub on_curve: bool,
}

impl Outline {
    /// Return the point slice for the contour at `index`.
    ///
    /// The returned slice references `points` and is invalidated when
    /// this outline is dropped.
    pub fn contour(&self, index: usize) -> &[Point] {
        let start = if index == 0 {
            0
        } else {
            self.contours[index - 1] as usize + 1
        };
        let end = self.contours[index] as usize + 1;
        &self.points[start..end]
    }
}

/// https://learn.microsoft.com/en-us/typography/opentype/spec/glyf#table-organization
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry<'a> {
    pub header: Header,

    /// We store a reference to the original bytes so that we can
    /// validate or iterate the contours or components of the glyph.
    ///
    /// This data starts immediately after the header.
    pub data: &'a [u8],
}

be_struct! {
    /// The header that's always present at
    /// the start of any glyph in the table.
    ///
    /// Depending on the number of contours, the data that
    /// comes afterwards must be interpreted differently.
    ///
    /// References:
    /// - https://learn.microsoft.com/en-us/typography/opentype/spec/glyf#glyph-headers
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct Header {
        /// If the number of contours is greater than
        /// or equal to zero, this is a simple glyph.
        ///
        /// If negative, this is a composite glyph — the
        /// value -1 should be used for composite glyphs.
        pub number_of_contours: Int16,

        /// Minimum x for coordinate data.
        pub x_min: Int16,

        /// Minimum y for coordinate data.
        pub y_min: Int16,

        /// Maximum x for coordinate data.
        pub x_max: Int16,

        /// Maximum y for coordinate data.
        pub y_max: Int16,
    }
}

impl Header {
    /// `@sizeOf(Header)`: the size of the header in the binary format.
    pub const SIZE: usize = 10;
}

/// The bit flags that describe the point data in simple glyph entries.
///
/// Doc strings for each field are copied with minimal modification
/// from the opentype spec. Field names are altered to be clearer and
/// more succinct, and mentions of those field names in doc strings
/// have been similarly modified to match the ones in the struct.
///
/// The relationship between the <x|y>_short and <x|y>_repeat_or_sign
/// fields is important, and poorly explained in prose, so instead of
/// that, here's a table that should make it easier to understand.
///
/// ```text
///          x_short > | false            | true             |
/// x_repeat_or_sign V |------------------|------------------|
///                    | The x-coordinate | The x-coordinate |
///              false | of this point is | of this point is |
///                    | a signed 16-bit  | an unsigned byte |
///                    | value added to   | value treated as |
///                    | the *Coordinates | negative, added  |
///                    | array.           | to the array of  |
///                    |                  | xCoordinates.    |
/// -------------------|------------------|------------------|
///                    | The x-coordinate | The x-coordinate |
///               true | of this point is | of this point is |
///                    | the same as the  | an unsigned byte |
///                    | previous point;  | value treated as |
///                    | nothing added to | positive, added  |
///                    | the xCoordinates | to the array of  |
///                    | array.           | xCoordinates.    |
/// -------------------|------------------|------------------|
/// ```
///
/// References:
/// - https://learn.microsoft.com/en-us/typography/opentype/spec/glyf#simple-glyph-description
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SimpleFlags {
    /// If set, the point is on the curve; otherwise, it is off the curve.
    pub on_curve: bool,

    /// If set, the corresponding x-coordinate is 1 byte long,
    /// and the sign is determined by the x_repeat_or_sign flag.
    ///
    /// If not set, its interpretation depends on the x_repeat_or_sign
    /// flag: If that other flag is set, the x-coordinate is the same
    /// as the previous x-coordinate, and no element is added to the
    /// xCoordinates array. If both flags are not set, the corresponding
    /// element in the xCoordinates array is two bytes and interpreted
    /// as a signed integer.
    ///
    /// See the description of the x_repeat_or_sign flag for additional
    /// information.
    pub x_short: bool,

    /// If set, the corresponding y-coordinate is 1 byte long,
    /// and the sign is determined by the y_repeat_or_sign flag.
    ///
    /// If not set, its interpretation depends on the y_repeat_or_sign
    /// flag: If that other flag is set, the y-coordinate is the same
    /// as the previous y-coordinate, and no element is added to the
    /// yCoordinates array. If both flags are not set, the corresponding
    /// element in the yCoordinates array is two bytes and interpreted
    /// as a signed integer.
    ///
    /// See the description of the y_repeat_or_sign flag for additional
    /// information.
    pub y_short: bool,

    /// If set, the next byte (read as unsigned) specifies the number
    /// of additional times this flag byte is to be repeated in the
    /// logical flags array — that is, the number of additional logical
    /// flag entries inserted after this entry. (In the expanded logical
    /// array, this bit is ignored.) In this way, the number of flags
    /// listed can be smaller than the number of points in the glyph
    /// description.
    pub repeat: bool,

    /// This flag has two meanings, depending on how the x_short flag
    /// is set. If x_short is set, this bit describes the sign of the
    /// value, with 1 equaling positive and 0 negative. If x_short is
    /// not set and this bit is set, then the current x-coordinate is
    /// the same as the previous x-coordinate. If x_short is not set
    /// and this bit is also not set, the current x-coordinate is a
    /// signed 16-bit delta vector.
    pub x_repeat_or_sign: bool,

    /// This flag has two meanings, depending on how the y_short flag
    /// is set. If y_short is set, this bit describes the sign of the
    /// value, with 1 equaling positive and 0 negative. If y_short is
    /// not set and this bit is set, then the current y-coordinate is
    /// the same as the previous y-coordinate. If y_short is not set
    /// and this bit is also not set, the current y-coordinate is a
    /// signed 16-bit delta vector.
    pub y_repeat_or_sign: bool,

    /// If set, contours in the glyph description could overlap.
    ///
    /// Use of this flag is not required — that is, contours may
    /// overlap without having this flag set. When used, it must
    /// be set on the first flag byte for the glyph.
    pub overlap: bool,

    /// Bit 7 is reserved: set to zero.
    pub reserved: bool,
}

impl SimpleFlags {
    /// Decode from the packed `u8` representation (`@bitCast`).
    #[inline]
    pub const fn from_bits(bits: u8) -> Self {
        Self {
            on_curve: bits & 0x01 != 0,
            x_short: bits & 0x02 != 0,
            y_short: bits & 0x04 != 0,
            repeat: bits & 0x08 != 0,
            x_repeat_or_sign: bits & 0x10 != 0,
            y_repeat_or_sign: bits & 0x20 != 0,
            overlap: bits & 0x40 != 0,
            reserved: bits & 0x80 != 0,
        }
    }

    /// Encode to the packed `u8` representation.
    #[inline]
    pub const fn to_bits(self) -> u8 {
        (self.on_curve as u8)
            | (self.x_short as u8) << 1
            | (self.y_short as u8) << 2
            | (self.repeat as u8) << 3
            | (self.x_repeat_or_sign as u8) << 4
            | (self.y_repeat_or_sign as u8) << 5
            | (self.overlap as u8) << 6
            | (self.reserved as u8) << 7
    }

    /// Determine the size (in bytes) of the corresponding
    /// value in the `xCoordinates` array for this flagset.
    ///
    /// See doc comments on the struct for an explanation.
    #[inline]
    pub fn x_bytes(self) -> u8 {
        if self.x_short {
            // short, 1 byte
            1
        } else if self.x_repeat_or_sign {
            // repeat, 0 bytes
            0
        } else {
            // otherwise, 16-bit, 2 bytes.
            2
        }
    }

    /// Determine the size (in bytes) of the corresponding
    /// value in the `yCoordinates` array for this flagset.
    ///
    /// See doc comments on the struct for an explanation.
    #[inline]
    pub fn y_bytes(self) -> u8 {
        if self.y_short {
            // short, 1 byte
            1
        } else if self.y_repeat_or_sign {
            // repeat, 0 bytes
            0
        } else {
            // otherwise, 16-bit, 2 bytes.
            2
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    /// A glyph made of standard contours.
    Simple,
    /// A glyph made of references to other glyphs.
    Composite,
}

/// Errors that can be returned from `Entry::size()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeError {
    /// The entry contains hinting instructions,
    /// which we don't currently support.
    InstructionsNotSupported,

    /// The entry is a composite glyph,
    /// which we don't currently support.
    CompositeNotSupported,

    /// The elements of the end points array
    /// must strictly monotonically increase.
    ///
    /// This error means the provided entry violated that.
    EndPointsOutOfOrder,

    /// This entry defines points past the index determined
    /// by the final element of the endPtsOfContours array.
    TooManyPoints,

    /// Reading past the end of the data.
    EndOfStream,
}

impl From<std::io::Error> for SizeError {
    fn from(e: std::io::Error) -> Self {
        ReadError::from(e).into()
    }
}

impl From<ReadError> for SizeError {
    fn from(e: ReadError) -> Self {
        match e {
            ReadError::EndOfStream => SizeError::EndOfStream,
        }
    }
}

impl fmt::Display for SizeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for SizeError {}

/// Errors that can be returned from `Entry::decode()`.
///
/// (The Zig error set also includes `OutOfMemory`; Rust allocation
/// failure aborts instead.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    InstructionsNotSupported,
    CompositeNotSupported,
    EndPointsOutOfOrder,
    TooManyPoints,
    EndOfStream,

    /// Coordinate delta accumulation overflowed.
    CoordinateOverflow,
}

impl From<std::io::Error> for DecodeError {
    fn from(e: std::io::Error) -> Self {
        ReadError::from(e).into()
    }
}

impl From<ReadError> for DecodeError {
    fn from(e: ReadError) -> Self {
        match e {
            ReadError::EndOfStream => DecodeError::EndOfStream,
        }
    }
}

impl From<SizeError> for DecodeError {
    fn from(e: SizeError) -> Self {
        match e {
            SizeError::InstructionsNotSupported => DecodeError::InstructionsNotSupported,
            SizeError::CompositeNotSupported => DecodeError::CompositeNotSupported,
            SizeError::EndPointsOutOfOrder => DecodeError::EndPointsOutOfOrder,
            SizeError::TooManyPoints => DecodeError::TooManyPoints,
            SizeError::EndOfStream => DecodeError::EndOfStream,
        }
    }
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for DecodeError {}

impl<'a> Entry<'a> {
    /// Initialize an entry from the provided data.
    ///
    /// This DOES NOT COPY the data, it only stores a pointer to it.
    ///
    /// The lifetime of this struct, then, is the same as the
    /// lifetime of the data that is used to initialize it.
    pub fn init(data: &'a [u8]) -> Result<Entry<'a>, ReadError> {
        let mut reader = Cursor::new(data);
        let header: Header = compat_reader::read_struct_endian(&mut reader, Endian::Big)?;
        Ok(Entry {
            header,
            data: &data[Header::SIZE..],
        })
    }

    /// Identifies what type (simple or composite) of entry this is.
    pub fn entry_type(&self) -> Type {
        if self.header.number_of_contours >= 0 {
            Type::Simple
        } else {
            Type::Composite
        }
    }

    /// Determines the size (in bytes) of this entry.
    ///
    /// If the entry is valid, returns the number of bytes
    /// taken up by this entry, including its header.
    ///
    /// NOTE: Currently produces errors when given composite glyphs
    ///       or any glyphs that have hinting instructions included.
    pub fn size(&self) -> Result<usize, SizeError> {
        let mut reader = Cursor::new(self.data);
        match self.entry_type() {
            // https://learn.microsoft.com/en-us/typography/opentype/spec/glyf#simple-glyph-description
            Type::Simple => {
                let num_contours = self.header.number_of_contours as usize;

                // From the spec:
                //
                // > If a glyph has zero contours, no additional glyph
                // > data beyond the header is required. A glyph with
                // > zero contours may have additional data, however;
                // > in particular, it may have instructions that
                // > operate on phantom points.
                //
                // If our number of contours is 0, and there's less than
                // two bytes in the remaining data, then we just return
                // the size of the header as our size. The reason for
                // two bytes is because that's the minimum size of the
                // extra data, since `instructionLength` is 16 bits.
                if num_contours == 0 && self.data.len() < 2 {
                    return Ok(Header::SIZE);
                }

                // uint16 endPtsOfContours[numberOfContours]
                //
                // Array of point indices for the last point
                // of each contour, in increasing numeric order.
                let mut max_point_index: isize = -1;
                for _ in 0..num_contours {
                    let index =
                        compat_reader::reader_int::<_, Uint16>(&mut reader, Endian::Big)? as isize;
                    // The endpoints are supposed to monotonically increase.
                    if index <= max_point_index {
                        return Err(SizeError::EndPointsOutOfOrder);
                    }
                    max_point_index = index;
                }

                // uint16 instructionLength
                //
                // Total number of bytes for instructions.
                //
                // If instructionLength is zero, no instructions
                // are present for this glyph, and this field is
                // followed directly by the flags field.
                let instructions_length =
                    compat_reader::reader_int::<_, Uint16>(&mut reader, Endian::Big)?;

                // Since we don't have code that validates instruction
                // byte code, we just reject all glyphs that contain any.
                //
                // In the future we could change this to just ignore the
                // instructions, or even validate them, but for now this
                // is fine, since we only need this function at all to
                // validate glyf entries from the glyph protocol, which
                // explicitly forbids instructions anyway.
                if instructions_length > 0 {
                    return Err(SizeError::InstructionsNotSupported);
                }

                // uint8 flags[variable]
                //
                // Array of flag elements.
                //
                // ---
                //
                // We do additional accounting here to figure out how many
                // bytes the next two fields (the [x|y]Coordinates arrays)
                // should take, so that we can just try to throw out that
                // many bytes in order to validate them. This is because
                // the length of each one depends on the flags.
                //
                // We're using `i` here to count the number of logical
                // entries we have, which should reach the number of
                // points defined by the final endpoint (from earlier).
                let mut i: usize = 0;
                let mut x_coords_len: usize = 0;
                let mut y_coords_len: usize = 0;
                while (i as isize) <= max_point_index {
                    let flag = SimpleFlags::from_bits(compat_reader::read_byte(&mut reader)?);

                    // Determine how many bytes the x and y coordinates will
                    // be represented with in the corresponding arrays, add
                    // them to our tallies.
                    x_coords_len += flag.x_bytes() as usize;
                    y_coords_len += flag.y_bytes() as usize;

                    // 0x08 REPEAT_FLAG
                    // Bit 3: If set, the next byte (read as unsigned)
                    // specifies the number of additional times this flag
                    // byte is to be repeated in the logical flags array
                    // — that is, the number of additional logical flag
                    // entries inserted after this entry.
                    if flag.repeat {
                        // The flag is repeated a certain number of times,
                        // which means that the point count is increased by
                        // that count, and the x_coords_len and y_coords_len
                        // must be increased by the correct number of bytes
                        // as well.
                        let repeat_count = compat_reader::read_byte(&mut reader)? as usize;
                        i += repeat_count;
                        x_coords_len += repeat_count * flag.x_bytes() as usize;
                        y_coords_len += repeat_count * flag.y_bytes() as usize;

                        // If the repeat count pushes our logical point
                        // number beyond the max point index which we
                        // figured out earlier from the end points, then
                        // there's an issue with this entry, error out.
                        if (i as isize) > max_point_index {
                            return Err(SizeError::TooManyPoints);
                        }
                    }

                    i += 1;
                }

                // uint8 or int16 xCoordinates[variable]
                //
                // Contour point x-coordinates.
                //
                // ---
                //
                // We determined the length of this section (in bytes)
                // above while processing the flags, so that we can just
                // skip that many bytes to validate this field.
                compat_reader::read_skip_bytes(&mut reader, x_coords_len as u64)?;

                // uint8 or int16 yCoordinates[variable]
                //
                // Contour point y-coordinates.
                //
                // ---
                //
                // We determined the length of this section (in bytes)
                // above while processing the flags, so that we can just
                // skip that many bytes to validate this field.
                compat_reader::read_skip_bytes(&mut reader, y_coords_len as u64)?;
            }

            Type::Composite => {
                // We don't have code for validating composite glyphs,
                // mainly because we don't need it, since we only use
                // this function for the glyph protocol which explicitly
                // forbids composite glyphs anyway.
                //
                // So we return false for composite glyphs.
                return Err(SizeError::CompositeNotSupported);
            }
        }

        // No issues found, the glyf entry is valid, return its length.
        Ok(Header::SIZE + reader.position() as usize)
    }

    /// Decode this simple glyph entry into an owned outline.
    ///
    /// NOTE: Currently produces errors when given composite glyphs
    ///       or any glyphs that have hinting instructions included.
    pub fn decode(&self) -> Result<Outline, DecodeError> {
        // We only support simple glyphs.
        match self.entry_type() {
            Type::Simple => {}
            Type::Composite => return Err(DecodeError::CompositeNotSupported),
        }

        let mut reader = Cursor::new(self.data);

        // A zero-contour glyph may be header-only. See size for the
        // reason for the hardcoded 2 here.
        let num_contours = self.header.number_of_contours as usize;
        if num_contours == 0 && self.data.len() < 2 {
            return Ok(Outline {
                points: Vec::new(),
                contours: Vec::new(),
            });
        }

        // We now know our full amount of contour ending points.
        let mut end_points: Vec<Uint16> = vec![0; num_contours];

        // If we have no contours, then the only possible remaining
        // field is instructionLength. Instructions are not supported.
        if num_contours == 0 {
            let instructions_length =
                compat_reader::reader_int::<_, Uint16>(&mut reader, Endian::Big)?;
            if instructions_length > 0 {
                return Err(DecodeError::InstructionsNotSupported);
            }
            return Ok(Outline {
                points: Vec::new(),
                contours: end_points,
            });
        }

        // The number of points is determined by the final end point
        // entry since the entries have to be monotonic (something
        // we verify below).
        let point_count: usize = {
            let mut prev_end_point: isize = -1;

            // Go through the end points array and update our end_points
            // with the valid index. The final endpoint tells us our point
            // count, since endpoints are stored as inclusive point indices.
            for end_point in end_points.iter_mut() {
                let index = compat_reader::reader_int::<_, Uint16>(&mut reader, Endian::Big)?;
                if index as isize <= prev_end_point {
                    return Err(DecodeError::EndPointsOutOfOrder);
                }
                prev_end_point = index as isize;
                *end_point = index;
            }

            // The final point tells us our point count.
            end_points[end_points.len() - 1] as usize + 1
        };

        // Instructions are not supported.
        let instructions_length = compat_reader::reader_int::<_, Uint16>(&mut reader, Endian::Big)?;
        if instructions_length > 0 {
            return Err(DecodeError::InstructionsNotSupported);
        }

        let mut points: Vec<Point> = vec![Point::default(); point_count];

        // This is EXTREMELY annoying but all the flags are separate
        // from the points so we have to do some allocation here since
        // its a dynamic amount and we need to save the values for later.
        //
        // Port note: the Zig code uses a 4096-byte stack buffer with a
        // heap fallback; we just use a Vec.
        let mut flags: Vec<SimpleFlags> = vec![SimpleFlags::default(); point_count];
        {
            let mut point_i: usize = 0;
            while point_i < point_count {
                let flag = SimpleFlags::from_bits(compat_reader::read_byte(&mut reader)?);
                flags[point_i] = flag;
                point_i += 1;

                if flag.repeat {
                    let repeat_count = compat_reader::read_byte(&mut reader)? as usize;
                    if point_i + repeat_count > point_count {
                        return Err(DecodeError::TooManyPoints);
                    }

                    for _ in 0..repeat_count {
                        flags[point_i] = flag;
                        point_i += 1;
                    }
                }
            }
        }

        // Go through x coordinate deltas
        let mut x: i32 = 0;
        for (flag, point) in flags.iter().zip(points.iter_mut()) {
            let dx: i32 = if flag.x_short {
                if flag.x_repeat_or_sign {
                    compat_reader::read_byte(&mut reader)? as i32
                } else {
                    -(compat_reader::read_byte(&mut reader)? as i32)
                }
            } else if !flag.x_repeat_or_sign {
                compat_reader::reader_int::<_, Int16>(&mut reader, Endian::Big)? as i32
            } else {
                0
            };

            x = x.checked_add(dx).ok_or(DecodeError::CoordinateOverflow)?;
            point.x = x;
        }

        // Go through y coordinate deltas
        let mut y: i32 = 0;
        for (flag, point) in flags.iter().zip(points.iter_mut()) {
            let dy: i32 = if flag.y_short {
                if flag.y_repeat_or_sign {
                    compat_reader::read_byte(&mut reader)? as i32
                } else {
                    -(compat_reader::read_byte(&mut reader)? as i32)
                }
            } else if !flag.y_repeat_or_sign {
                compat_reader::reader_int::<_, Int16>(&mut reader, Endian::Big)? as i32
            } else {
                0
            };

            y = y.checked_add(dy).ok_or(DecodeError::CoordinateOverflow)?;
            point.y = y;
            point.on_curve = flag.on_curve;
        }

        Ok(Outline {
            points,
            contours: end_points,
        })
    }
}

impl<'a> Glyf<'a> {
    /// Initialize the table from the provided data.
    ///
    /// This DOES NOT COPY the data, it only stores a pointer to it.
    ///
    /// The lifetime of this struct, then, is the same as the
    /// lifetime of the data that is used to initialize it.
    pub fn init(data: &'a [u8]) -> Glyf<'a> {
        Glyf { data }
    }

    /// Retrieve the entry at the provided offset.
    pub fn entry(&self, index: usize) -> Result<Entry<'a>, ReadError> {
        Entry::init(&self.data[index..])
    }
}

/// TESTING ONLY
///
/// Retrieves the glyf at the provided index from the provided font.
///
/// Returns it in a tuple with the expected length based on the loca table, and the entry.
#[cfg(test)]
pub fn get_glyph<'a>(
    font: &super::sfnt::Sfnt<'a>,
    index: usize,
) -> Result<(usize, Entry<'a>), ReadError> {
    let glyf = Glyf::init(font.get_table(b"glyf").unwrap());
    let head = super::head::Head::init(font.get_table(b"head").unwrap())?;
    let loca = font.get_table(b"loca").unwrap();

    let read_offset = |i: usize| -> usize {
        match head.index_to_loc_format {
            0 => u16::from_be_bytes([loca[i * 2], loca[i * 2 + 1]]) as usize * 2,
            1 => u32::from_be_bytes(loca[i * 4..i * 4 + 4].try_into().unwrap()) as usize,
            _ => unreachable!(),
        }
    };

    let start_offset = read_offset(index);
    let end_offset = read_offset(index + 1);

    Ok((end_offset - start_offset, glyf.entry(start_offset)?))
}

#[cfg(test)]
fn test_append_header(
    buf: &mut Vec<u8>,
    number_of_contours: i16,
    x_min: i16,
    y_min: i16,
    x_max: i16,
    y_max: i16,
) {
    buf.extend_from_slice(&number_of_contours.to_be_bytes());
    buf.extend_from_slice(&x_min.to_be_bytes());
    buf.extend_from_slice(&y_min.to_be_bytes());
    buf.extend_from_slice(&x_max.to_be_bytes());
    buf.extend_from_slice(&y_max.to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::super::sfnt::Sfnt;
    use super::*;

    use crate::terminal::build_options::{self, Artifact};

    /// `@import("terminal_options").artifact == .lib`
    const ARTIFACT_IS_LIB: bool = matches!(build_options::ARTIFACT, Artifact::Lib);

    /// Port of `testAppendInt` (big-endian append).
    macro_rules! append_int {
        ($buf:expr, $t:ty, $v:expr) => {
            $buf.extend_from_slice(&(<$t>::to_be_bytes($v)))
        };
    }

    #[test]
    fn glyf() {
        // lib-vt source archives intentionally exclude full Ghostty font fixtures.
        if ARTIFACT_IS_LIB {
            return;
        }

        // Cozette because it doesn't have any hinting.
        let test_font = crate::font::embedded::COZETTE;

        let font = Sfnt::init(test_font).unwrap();

        // Cozette doesn't actually include a glyph for notdef,
        // but does include a glyph for `\0` (nul), at index 1.
        let (len_nul, glyph_nul) = get_glyph(&font, 1).unwrap();
        assert!(glyph_nul.entry_type() == Type::Simple);
        // It is legal for there to be extra data between two entries, just
        // as long as the next entry starts after the previous one ends, so
        // it's okay for the parsed size of the entry to be less than the size
        // determined from the difference between subsequent loca offsets.
        assert!(len_nul >= glyph_nul.size().unwrap());

        // Glyph "A" is at index 66.
        let (len_a, glyph_a) = get_glyph(&font, 66).unwrap();
        assert!(glyph_a.entry_type() == Type::Simple);
        assert!(len_a >= glyph_a.size().unwrap());

        let outline_a = glyph_a.decode().unwrap();
        assert_eq!(
            glyph_a.header.number_of_contours as usize,
            outline_a.contours.len()
        );
        assert!(!outline_a.points.is_empty());

        // Glyph "Ĩ" is at index 265.
        let (len_itilde, glyph_itilde) = get_glyph(&font, 265).unwrap();
        assert!(glyph_itilde.entry_type() == Type::Simple);
        assert!(len_itilde >= glyph_itilde.size().unwrap());
    }

    #[test]
    fn glyf_decode_triangle() {
        let mut buf: Vec<u8> = Vec::new();

        test_append_header(&mut buf, 1, 100, 100, 900, 900);
        append_int!(buf, u16, 2); // endPtsOfContours[0]
        append_int!(buf, u16, 0); // instructionLength
        buf.push(0x01); // on curve
        buf.push(0x01); // on curve
        buf.push(0x01); // on curve
        append_int!(buf, i16, 500);
        append_int!(buf, i16, -400);
        append_int!(buf, i16, 800);
        append_int!(buf, i16, 900);
        append_int!(buf, i16, -800);
        append_int!(buf, i16, 0);

        let glyph = Entry::init(&buf).unwrap();
        let outline = glyph.decode().unwrap();

        assert_eq!(100i16, glyph.header.x_min);
        assert_eq!(900i16, glyph.header.x_max);
        assert_eq!(1usize, outline.contours.len());
        assert_eq!(3usize, outline.points.len());
        let contour = outline.contour(0);
        assert_eq!(3usize, contour.len());
        assert_eq!(Point { x: 500, y: 900, on_curve: true }, contour[0]);
        assert_eq!(Point { x: 100, y: 100, on_curve: true }, contour[1]);
        assert_eq!(Point { x: 900, y: 100, on_curve: true }, contour[2]);
    }

    #[test]
    fn glyf_decode_multiple_contours() {
        let mut buf: Vec<u8> = Vec::new();

        test_append_header(&mut buf, 2, 0, 0, 30, 10);
        append_int!(buf, u16, 1); // first contour ends at point 1
        append_int!(buf, u16, 3); // second contour ends at point 3
        append_int!(buf, u16, 0); // instructionLength
        for _ in 0..4 {
            buf.push(0x01); // on curve
        }
        for dx in [0i16, 10, 10, 10] {
            append_int!(buf, i16, dx);
        }
        for dy in [0i16, 0, 10, 0] {
            append_int!(buf, i16, dy);
        }

        let glyph = Entry::init(&buf).unwrap();
        let outline = glyph.decode().unwrap();

        assert_eq!(2usize, outline.contours.len());
        assert_eq!(4usize, outline.points.len());
        assert_eq!(1u16, outline.contours[0]);
        assert_eq!(3u16, outline.contours[1]);
        assert_eq!(2usize, outline.contour(0).len());
        assert_eq!(2usize, outline.contour(1).len());
        assert_eq!(outline.points[0..2].as_ptr(), outline.contour(0).as_ptr());
        assert_eq!(outline.points[2..4].as_ptr(), outline.contour(1).as_ptr());
    }

    #[test]
    fn glyf_decode_repeat_and_short_vector_flags() {
        let mut buf: Vec<u8> = Vec::new();

        test_append_header(&mut buf, 1, 0, -16, 16, 0);
        append_int!(buf, u16, 3); // four points
        append_int!(buf, u16, 0); // instructionLength
        buf.push(0x01 | 0x02 | 0x04 | 0x08 | 0x10); // on, x short positive, y short negative, repeat
        buf.push(3); // repeat for the next three points
        for dx in [1u8, 2, 4, 8] {
            buf.push(dx);
        }
        for dy in [1u8, 2, 4, 8] {
            buf.push(dy);
        }

        let glyph = Entry::init(&buf).unwrap();
        let outline = glyph.decode().unwrap();

        assert_eq!(4usize, outline.points.len());
        assert_eq!(Point { x: 1, y: -1, on_curve: true }, outline.points[0]);
        assert_eq!(Point { x: 3, y: -3, on_curve: true }, outline.points[1]);
        assert_eq!(Point { x: 7, y: -7, on_curve: true }, outline.points[2]);
        assert_eq!(Point { x: 15, y: -15, on_curve: true }, outline.points[3]);
    }

    #[test]
    fn glyf_decode_off_curve_and_same_coordinate_flags() {
        let mut buf: Vec<u8> = Vec::new();

        test_append_header(&mut buf, 1, 0, 0, 7, 9);
        append_int!(buf, u16, 1); // two points
        append_int!(buf, u16, 0); // instructionLength
        buf.push(0x10 | 0x20); // off curve, x same, y same
        buf.push(0x01 | 0x02 | 0x04 | 0x10 | 0x20); // on curve, short positive x/y
        buf.push(7); // x delta
        buf.push(9); // y delta

        let glyph = Entry::init(&buf).unwrap();
        let outline = glyph.decode().unwrap();

        assert_eq!(Point { x: 0, y: 0, on_curve: false }, outline.points[0]);
        assert_eq!(Point { x: 7, y: 9, on_curve: true }, outline.points[1]);
    }

    #[test]
    fn glyf_decode_one_point_contour() {
        let mut buf: Vec<u8> = Vec::new();

        test_append_header(&mut buf, 1, 0, 0, 0, 0);
        append_int!(buf, u16, 0); // endPtsOfContours[0]
        append_int!(buf, u16, 0); // instructionLength
        buf.push(0x01 | 0x10 | 0x20); // on curve, x same, y same

        let glyph = Entry::init(&buf).unwrap();
        let outline = glyph.decode().unwrap();

        assert_eq!(1usize, outline.points.len());
        assert_eq!(1usize, outline.contours.len());
        assert_eq!(0u16, outline.contours[0]);
        assert_eq!(Point { x: 0, y: 0, on_curve: true }, outline.contour(0)[0]);
    }

    #[test]
    fn glyf_decode_contour_ending_at_max_point_index() {
        let mut buf: Vec<u8> = Vec::new();

        test_append_header(&mut buf, 1, 0, 0, 0, 0);
        append_int!(buf, u16, u16::MAX); // 65536 points
        append_int!(buf, u16, 0); // instructionLength

        let flag: u8 = 0x01 | 0x10 | 0x20; // on curve, x same, y same
        let mut remaining: usize = u16::MAX as usize + 1;
        while remaining > 0 {
            let run = remaining.min(256);
            if run == 1 {
                buf.push(flag);
            } else {
                buf.push(flag | 0x08); // repeat
                buf.push((run - 1) as u8);
            }
            remaining -= run;
        }

        let glyph = Entry::init(&buf).unwrap();
        let outline = glyph.decode().unwrap();

        assert_eq!(65536usize, outline.points.len());
        assert_eq!(65536usize, outline.contour(0).len());
    }

    #[test]
    fn glyf_reject_glyphs_with_instructions_and_composite_glyphs() {
        // lib-vt source archives intentionally exclude full Ghostty font fixtures.
        if ARTIFACT_IS_LIB {
            return;
        }

        let test_font = crate::font::embedded::JETBRAINS_MONO;

        let font = Sfnt::init(test_font).unwrap();

        let (len_notdef, glyph_notdef) = get_glyph(&font, 0).unwrap();
        assert_eq!(100, len_notdef);
        assert!(glyph_notdef.entry_type() == Type::Simple);
        assert_eq!(Err(SizeError::InstructionsNotSupported), glyph_notdef.size());
        assert_eq!(
            Err(DecodeError::InstructionsNotSupported),
            glyph_notdef.decode()
        );

        // Glyph "Á" is at index 2.
        let (len_aacute, glyph_aacute) = get_glyph(&font, 2).unwrap();
        assert_eq!(24, len_aacute);
        assert!(glyph_aacute.entry_type() == Type::Composite);
        assert_eq!(Err(SizeError::CompositeNotSupported), glyph_aacute.size());
        assert_eq!(
            Err(DecodeError::CompositeNotSupported),
            glyph_aacute.decode()
        );
    }

    #[test]
    fn glyf_reject_truncated() {
        // lib-vt source archives intentionally exclude full Ghostty font fixtures.
        if ARTIFACT_IS_LIB {
            return;
        }

        // Cozette because it doesn't have any hinting.
        let test_font = crate::font::embedded::COZETTE;

        let font = Sfnt::init(test_font).unwrap();

        let (_, mut glyph_nul) = get_glyph(&font, 1).unwrap();
        assert!(glyph_nul.entry_type() == Type::Simple);
        // Mess with the entry's data slice, truncating
        // it before the full length (which is 228 bytes).
        glyph_nul.data = &glyph_nul.data[0..227 - Header::SIZE];
        assert_eq!(Err(SizeError::EndOfStream), glyph_nul.size());
        assert_eq!(Err(DecodeError::EndOfStream), glyph_nul.decode());
    }

    #[test]
    fn glyf_reject_endpoints_out_of_order() {
        // lib-vt source archives intentionally exclude full Ghostty font fixtures.
        if ARTIFACT_IS_LIB {
            return;
        }

        // Cozette because it doesn't have any hinting.
        //
        // Also we copy it so we can mess with it.
        let mut test_font = crate::font::embedded::COZETTE.to_vec();

        // Find the entry's data offset in the copied font so we can mutate it.
        let data_offset = {
            let font = Sfnt::init(&test_font).unwrap();
            let (_, glyph_nul) = get_glyph(&font, 1).unwrap();
            assert!(glyph_nul.entry_type() == Type::Simple);
            glyph_nul.data.as_ptr() as usize - font.data.as_ptr() as usize
        };

        // Mess with the entry's data, insert a 0 in the middle of the endpoints
        // (u16 index 3 of the entry data).
        test_font[data_offset + 6] = 0;
        test_font[data_offset + 7] = 0;

        let font = Sfnt::init(&test_font).unwrap();
        let (_, glyph_nul) = get_glyph(&font, 1).unwrap();
        assert_eq!(Err(SizeError::EndPointsOutOfOrder), glyph_nul.size());
        assert_eq!(Err(DecodeError::EndPointsOutOfOrder), glyph_nul.decode());
    }

    #[test]
    fn glyf_reject_too_many_points() {
        // lib-vt source archives intentionally exclude full Ghostty font fixtures.
        if ARTIFACT_IS_LIB {
            return;
        }

        // Cozette because it doesn't have any hinting.
        //
        // Also we copy it so we can mess with it.
        let mut test_font = crate::font::embedded::COZETTE.to_vec();

        let data_offset = {
            let font = Sfnt::init(&test_font).unwrap();
            let (_, glyph_nul) = get_glyph(&font, 1).unwrap();
            assert!(glyph_nul.entry_type() == Type::Simple);
            glyph_nul.data.as_ptr() as usize - font.data.as_ptr() as usize
        };

        // Mess with the entry's data, make the final two bytes of the flags
        // array be a large number repeat to exceed the correct points count.
        test_font[data_offset + 107] |= 0x08;
        test_font[data_offset + 108] = 0xFF;

        let font = Sfnt::init(&test_font).unwrap();
        let (_, glyph_nul) = get_glyph(&font, 1).unwrap();
        assert_eq!(Err(SizeError::TooManyPoints), glyph_nul.size());
        assert_eq!(Err(DecodeError::TooManyPoints), glyph_nul.decode());
    }

    #[test]
    fn glyf_zero_contour_glyph_can_be_header_only() {
        let header = Header {
            number_of_contours: 0,
            x_min: 0,
            y_min: 0,
            x_max: 0,
            y_max: 0,
        };
        let mut bytes: Vec<u8> = Vec::new();
        test_append_header(
            &mut bytes,
            header.number_of_contours,
            header.x_min,
            header.y_min,
            header.x_max,
            header.y_max,
        );
        let glyph = Entry::init(&bytes).unwrap();
        assert_eq!(Header::SIZE, glyph.size().unwrap());

        let outline = glyph.decode().unwrap();
        assert_eq!(0usize, outline.points.len());
        assert_eq!(0usize, outline.contours.len());
    }

    #[test]
    fn glyf_zero_contour_glyph_can_include_instruction_length() {
        let mut buf: Vec<u8> = Vec::new();

        test_append_header(&mut buf, 0, 0, 0, 0, 0);
        append_int!(buf, u16, 0); // instructionLength

        let glyph = Entry::init(&buf).unwrap();
        assert_eq!(Header::SIZE + 2, glyph.size().unwrap());

        let outline = glyph.decode().unwrap();
        assert_eq!(0usize, outline.points.len());
        assert_eq!(0usize, outline.contours.len());
    }

    #[test]
    fn glyf_zero_contour_glyph_rejects_instructions() {
        let mut buf: Vec<u8> = Vec::new();

        test_append_header(&mut buf, 0, 0, 0, 0, 0);
        append_int!(buf, u16, 1); // instructionLength

        let glyph = Entry::init(&buf).unwrap();
        assert_eq!(Err(SizeError::InstructionsNotSupported), glyph.size());
        assert_eq!(Err(DecodeError::InstructionsNotSupported), glyph.decode());
    }
}
