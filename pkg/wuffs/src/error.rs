//! Port of `src/wuffs/src/error.zig`.
//!
//! STATUS: ported
//!
//! The Zig package wraps the Wuffs C library. The Rust port implements the
//! same API with pure-Rust decoders (`png`, `zune-jpeg`, `miniz_oxide`) and
//! hand-written swizzlers that reproduce Wuffs' arithmetic, so there is no
//! `wuffs_base__status` here: [`check`] takes the decoder's `Result` instead.

use std::fmt;

/// `std.mem.Allocator.Error || error{ WuffsError, Overflow }`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    OutOfMemory,
    WuffsError,
    Overflow,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Error::OutOfMemory => "OutOfMemory",
            Error::WuffsError => "WuffsError",
            Error::Overflow => "Overflow",
        })
    }
}

impl std::error::Error for Error {}

/// Check the status of a decode operation, logging the error message to the
/// given log scope (target) and converting it to [`Error::WuffsError`].
pub fn check<T, E: fmt::Display>(log: &str, status: Result<T, E>) -> Result<T, Error> {
    match status {
        Ok(v) => Ok(v),
        Err(e) => {
            log::warn!(target: log, "decode err={e}");
            Err(Error::WuffsError)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_ok_and_err() {
        assert_eq!(check::<_, String>("wuffs_test", Ok(1)), Ok(1));
        assert_eq!(
            check::<u8, _>("wuffs_test", Err("bad")),
            Err(Error::WuffsError)
        );
    }
}
