//! Port of `src/lib/compat/file.zig`.
//!
//! STATUS: ported
//!
//! Code taken from Zig 0.15.2 and `std.fs.file`. See README.md for license
//! and details. In Rust this maps onto `std::io::Read`; the only addition
//! over `Read::read_to_end` is the `max_bytes` limit.

use std::fmt;
use std::io::{self, Read};

#[derive(Debug)]
pub enum ReadToEndAllocError {
    FileTooBig,
    Io(io::Error),
}

impl fmt::Display for ReadToEndAllocError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadToEndAllocError::FileTooBig => f.write_str("FileTooBig"),
            ReadToEndAllocError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ReadToEndAllocError {}

impl From<io::Error> for ReadToEndAllocError {
    fn from(e: io::Error) -> Self {
        ReadToEndAllocError::Io(e)
    }
}

/// Read the file from its current position through end-of-stream, returning
/// `FileTooBig` if the result exceeds `max_bytes`.
pub fn read_to_end_alloc<R: Read>(
    file: &mut R,
    max_bytes: usize,
) -> Result<Vec<u8>, ReadToEndAllocError> {
    let mut read_buf = [0u8; 4096];
    let mut result: Vec<u8> = Vec::new();

    loop {
        let n = match file.read(&mut read_buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        };
        if n > max_bytes - result.len() {
            return Err(ReadToEndAllocError::FileTooBig);
        }
        result.extend_from_slice(&read_buf[..n]);
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::os::temp_dir::TempDir;

    #[test]
    fn read_to_end_alloc_reads_through_eof_and_permits_exact_limit() {
        let contents = "hello, world";

        let tmp_dir = TempDir::init().unwrap();
        let path = tmp_dir.path().join("data");
        std::fs::write(&path, contents).unwrap();

        let mut file = std::fs::File::open(&path).unwrap();
        let result = read_to_end_alloc(&mut file, contents.len()).unwrap();
        assert_eq!(contents.as_bytes(), &result[..]);

        let mut too_big_file = std::fs::File::open(&path).unwrap();
        assert!(matches!(
            read_to_end_alloc(&mut too_big_file, contents.len() - 1),
            Err(ReadToEndAllocError::FileTooBig)
        ));
    }
}
