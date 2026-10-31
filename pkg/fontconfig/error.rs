//! Port of `pkg/fontconfig/error.zig`.
//!
//! STATUS: ported

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    OutOfMemory,
    FontconfigFailed,
    FontconfigNoMatch,
    FontconfigTypeMismatch,
    FontconfigNoId,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for Error {}
