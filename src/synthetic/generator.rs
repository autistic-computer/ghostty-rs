//! Port of `src/synthetic/Generator.zig`.
//!
//! STATUS: ported
//!
//! A common interface for all generators.
//!
//! Zig implements this as a type-erased vtable (`ptr` + `nextFn`) built by
//! `Generator.init`. In Rust the interface is a trait; a generator
//! implementation's `generator()` method returns `&mut dyn Generator`.

use std::fmt;
use std::io;

/// For generators, this is the only error that is allowed to be
/// returned by the next function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    WriteFailed,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WriteFailed")
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(_: io::Error) -> Self {
        Error::WriteFailed
    }
}

/// The writer type generators write to (Zig `*std.Io.Writer`).
pub type Writer<'a> = dyn io::Write + 'a;

/// A common interface for all generators.
pub trait Generator {
    /// Get the next value from the generator, writing it to `writer`.
    fn next(&mut self, writer: &mut Writer<'_>, max_len: usize) -> Result<(), Error>;
}

/// Zig `std.Io.Writer.fixed`: a writer into a fixed buffer. Writes that
/// don't fit copy as much as possible and then fail (`WriteFailed`).
#[derive(Debug)]
pub struct FixedWriter<'a> {
    buffer: &'a mut [u8],
    end: usize,
}

impl<'a> FixedWriter<'a> {
    pub fn fixed(buffer: &'a mut [u8]) -> FixedWriter<'a> {
        FixedWriter { buffer, end: 0 }
    }

    /// The bytes written so far.
    pub fn buffered(&self) -> &[u8] {
        &self.buffer[..self.end]
    }
}

impl io::Write for FixedWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let dest = &mut self.buffer[self.end..];
        if dest.is_empty() && !buf.is_empty() {
            return Err(io::Error::new(io::ErrorKind::WriteZero, "WriteFailed"));
        }
        let len = buf.len().min(dest.len());
        dest[..len].copy_from_slice(&buf[..len]);
        self.end += len;
        Ok(len)
    }

    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        let dest = &mut self.buffer[self.end..];
        let len = buf.len().min(dest.len());
        dest[..len].copy_from_slice(&buf[..len]);
        self.end += len;
        if buf.len() > len {
            return Err(io::Error::new(io::ErrorKind::WriteZero, "WriteFailed"));
        }
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn fixed_writer_partial() {
        let mut buf = [0u8; 4];
        let mut w = FixedWriter::fixed(&mut buf);
        w.write_all(b"ab").unwrap();
        assert!(w.write_all(b"cde").is_err());
        assert_eq!(b"abcd", w.buffered());
    }
}
