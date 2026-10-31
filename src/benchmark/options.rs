//! Port of `src/benchmark/options.zig`.
//!
//! STATUS: ported
//!
//! This file contains helpers for CLI options.

use std::fs;
use std::io::{self, Read};

/// An open data file (Zig: `std.Io.File`, which can also be stdin).
/// Closing the file is dropping it.
#[derive(Debug)]
pub enum DataFile {
    Stdin(io::Stdin),
    File(fs::File),
}

impl Read for DataFile {
    #[inline]
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            DataFile::Stdin(s) => s.read(buf),
            DataFile::File(f) => f.read(buf),
        }
    }
}

/// Returns the data file for the given path in a way that is consistent
/// across our CLI. If the path is not set then no file is returned.
/// If the path is "-", then we will return stdin. If the path is
/// a file then we will open and return the handle.
pub fn data_file(path_: Option<&str>) -> io::Result<Option<DataFile>> {
    let Some(path) = path_ else {
        return Ok(None);
    };

    // Stdin
    if path == "-" {
        return Ok(Some(DataFile::Stdin(io::stdin())));
    }

    // Normal file
    let file = fs::File::open(path)?;
    Ok(Some(DataFile::File(file)))
}

/// Read until `buf` is full or the reader reaches end of stream,
/// returning the number of bytes read (Zig: `std.Io.Reader.readSliceShort`,
/// used by every benchmark that streams its data file).
pub fn read_slice_short<R: Read + ?Sized>(r: &mut R, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_file_null() {
        assert!(data_file(None).unwrap().is_none());
    }

    #[test]
    fn data_file_stdin() {
        assert!(matches!(data_file(Some("-")).unwrap(), Some(DataFile::Stdin(_))));
    }

    #[test]
    fn data_file_missing() {
        assert!(data_file(Some("/nonexistent/ghostty-bench-data")).is_err());
    }

    #[test]
    fn read_slice_short_reads_to_eof() {
        let mut r: &[u8] = b"hello";
        let mut buf = [0u8; 8];
        assert_eq!(5, read_slice_short(&mut r, &mut buf).unwrap());
        assert_eq!(b"hello", &buf[..5]);
        assert_eq!(0, read_slice_short(&mut r, &mut buf).unwrap());
    }
}
