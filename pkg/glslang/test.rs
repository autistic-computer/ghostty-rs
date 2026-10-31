//! Port of `src/glslang/test.zig`.
//!
//! STATUS: ported

use std::sync::Once;

use super::Error;

static INIT: Once = Once::new();

/// Call this function before any other tests in this package to ensure that
/// the glslang library is initialized.
pub fn ensure_init() -> Result<(), Error> {
    let mut result = Ok(());
    INIT.call_once(|| result = super::init());
    result
}
