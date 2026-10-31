//! Port of `src/lib/compat/reader.zig`.
//!
//! STATUS: ported
//!
//! Code taken from older reader implementations, still may be useful to keep
//! around in general. See README.md for license and details. The Rust port
//! operates on any `std::io::Read`; Zig's `error.EndOfStream` is
//! `io::ErrorKind::UnexpectedEof`.

use std::io::{self, Read};

/// Byte order (Zig: `std.lang.Endian`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endian {
    Big,
    Little,
}

/// Types that can be read with an explicit byte order. Implemented for
/// all primitive integers; implement it for `extern struct` ports to use
/// [`read_struct_endian`] (byte-swap each field like Zig's
/// `readSliceEndian`).
pub trait ReadEndian: Sized {
    fn read_endian<R: Read + ?Sized>(reader: &mut R, endian: Endian) -> io::Result<Self>;
}

macro_rules! impl_read_endian {
    ($($t:ty),*) => {
        $(impl ReadEndian for $t {
            fn read_endian<R: Read + ?Sized>(reader: &mut R, endian: Endian) -> io::Result<Self> {
                let bytes = read_bytes_no_eof::<R, { std::mem::size_of::<$t>() }>(reader)?;
                Ok(match endian {
                    Endian::Big => <$t>::from_be_bytes(bytes),
                    Endian::Little => <$t>::from_le_bytes(bytes),
                })
            }
        })*
    };
}

impl_read_endian!(u8, u16, u32, u64, u128, i8, i16, i32, i64, i128);

/// Byte arrays (e.g. table tags) have no byte order and are read as is,
/// like `[N]u8` fields of an extern struct in Zig's `readStructEndian`.
impl<const N: usize> ReadEndian for [u8; N] {
    fn read_endian<R: Read + ?Sized>(reader: &mut R, _endian: Endian) -> io::Result<Self> {
        read_bytes_no_eof::<R, N>(reader)
    }
}

/// Pulled from old stdlib as this function has been removed.
pub fn read_skip_until_delimiter_or_eof<R: Read + ?Sized>(
    reader: &mut R,
    delimiter: u8,
) -> io::Result<()> {
    loop {
        let byte = match read_byte(reader) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e),
        };
        if byte == delimiter {
            return Ok(());
        }
    }
}

/// Pulled from old stdlib as this function has been removed.
pub fn read_struct_endian<R: Read + ?Sized, T: ReadEndian>(
    reader: &mut R,
    endian: Endian,
) -> io::Result<T> {
    T::read_endian(reader, endian)
}

/// Pulled from old stdlib as this function has been removed.
pub fn reader_int<R: Read + ?Sized, T: ReadEndian>(
    reader: &mut R,
    endian: Endian,
) -> io::Result<T> {
    T::read_endian(reader, endian)
}

/// Pulled from old stdlib as this function has been removed.
pub fn read_byte_signed<R: Read + ?Sized>(reader: &mut R) -> io::Result<i8> {
    Ok(read_byte(reader)? as i8)
}

/// Pulled from old stdlib as this function has been removed.
pub fn read_byte<R: Read + ?Sized>(reader: &mut R) -> io::Result<u8> {
    let mut result = [0u8; 1];
    reader.read_exact(&mut result)?;
    Ok(result[0])
}

/// Pulled from old stdlib as this function has been removed.
pub fn read_skip_bytes<R: Read + ?Sized>(reader: &mut R, num_bytes: u64) -> io::Result<()> {
    const BUF_SIZE: usize = 512;

    let mut buf = [0u8; BUF_SIZE];
    let mut remaining = num_bytes;

    while remaining > 0 {
        let amt = remaining.min(BUF_SIZE as u64) as usize;
        reader.read_exact(&mut buf[..amt])?;
        remaining -= amt as u64;
    }
    Ok(())
}

/// Pulled from old stdlib as this function has been removed.
fn read_bytes_no_eof<R: Read + ?Sized, const N: usize>(reader: &mut R) -> io::Result<[u8; N]> {
    let mut bytes = [0u8; N];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reader_helpers() {
        let data: &[u8] = &[1, 2, 0xff, 0x12, 0x34, 0x12, 0x34, b'a', b'\n', 9, 9, 9, 7];
        let mut r = data;
        assert_eq!(1, read_byte(&mut r).unwrap());
        assert_eq!(2, read_byte(&mut r).unwrap());
        assert_eq!(-1, read_byte_signed(&mut r).unwrap());
        assert_eq!(
            0x1234u16,
            reader_int::<_, u16>(&mut r, Endian::Big).unwrap()
        );
        assert_eq!(
            0x3412u16,
            read_struct_endian::<_, u16>(&mut r, Endian::Little).unwrap()
        );
        read_skip_until_delimiter_or_eof(&mut r, b'\n').unwrap();
        read_skip_bytes(&mut r, 3).unwrap();
        assert_eq!(7, read_byte(&mut r).unwrap());
        assert_eq!(
            io::ErrorKind::UnexpectedEof,
            read_byte(&mut r).unwrap_err().kind()
        );
        // Skipping to a delimiter that never comes stops at EOF.
        read_skip_until_delimiter_or_eof(&mut r, b'\n').unwrap();
    }
}
