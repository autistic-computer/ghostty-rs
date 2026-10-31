//! Port of `pkg/harfbuzz/errors.zig`.
//!
//! STATUS: ported

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Not very descriptive but harfbuzz doesn't actually have error
    /// codes so the best we can do!
    HarfbuzzFailed,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for Error {}
