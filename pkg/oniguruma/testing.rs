//! Port of `pkg/oniguruma/testing.zig`.
//!
//! STATUS: ported

use std::sync::OnceLock;

use super::errors::Error;
use super::init;
use super::types::Encoding;

static INITIALIZED: OnceLock<Result<(), Error>> = OnceLock::new();

/// Call this function before any other tests in this package to ensure that
/// the oni library is initialized. This should only be used for tests
/// and only when you're sure this is the ONLY way that oni is being
/// initialized.
///
/// This always only initializes the encodings the tests use.
///
/// (The Zig version guards with a plain global `bool`; Rust tests run in
/// parallel threads, so a `OnceLock` is used.)
pub fn ensure_init() -> Result<(), Error> {
    *INITIALIZED.get_or_init(|| init::init(&[Encoding::utf8()]))
}
