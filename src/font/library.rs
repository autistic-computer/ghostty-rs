//! Port of `src/font/library.zig`.
//!
//! STATUS: ported
//!
//! A library represents the shared state that the underlying font
//! library implementation(s) require per-process.

use std::sync::{Arc, Mutex};

use super::backend::Backend;
use crate::pkg::freetype;

/// Library implementation for the compile options.
#[cfg(not(any(target_os = "macos", target_os = "ios", target_arch = "wasm32")))]
pub type Library = FreetypeLibrary;

/// Library implementation for the compile options.
///
/// Some backends such as CT and Canvas don't have a "library".
#[cfg(any(target_os = "macos", target_os = "ios", target_arch = "wasm32"))]
pub type Library = NoopLibrary;

const _: () = {
    // Keep the `Library` alias in sync with the configured backend.
    let has_ft = crate::font::OPTIONS.backend.has_freetype();
    let is_ft = cfg!(not(any(target_os = "macos", target_os = "ios", target_arch = "wasm32")));
    assert!(has_ft == is_ft);
    let _ = Backend::Freetype;
};

/// Library backed by FreeType.
///
/// This is cheap to clone: clones share the same FreeType library and
/// mutex (the Zig struct is copied by value and holds pointers).
#[derive(Debug, Clone)]
pub struct FreetypeLibrary {
    pub lib: freetype::Library,

    /// Mutex to be held any time the library is
    /// being used to create or destroy a face.
    pub mutex: Arc<Mutex<()>>,
}

// SAFETY: an FT_Library handle may be used from multiple threads as long as
// face creation/destruction is serialized, which `mutex` is for (this is
// how the Zig code shares the library between threads).
unsafe impl Send for FreetypeLibrary {}
unsafe impl Sync for FreetypeLibrary {}

pub type FreetypeInitError = freetype::Error;

impl FreetypeLibrary {
    pub fn init() -> Result<FreetypeLibrary, FreetypeInitError> {
        let lib = freetype::Library::init()?;
        let mutex = Arc::new(Mutex::new(()));
        Ok(FreetypeLibrary { lib, mutex })
    }

    /// Release the FreeType library.
    ///
    /// Like the Zig version, this must be called exactly once (on one of
    /// the clones), after every face created from this library has been
    /// deinitialized.
    pub fn deinit(&mut self) {
        // SAFETY: the library is not used after deinit (caller contract,
        // same as the Zig `deinit`).
        unsafe { self.lib.deinit() };
    }
}

/// Library for backends without any library state.
#[derive(Debug, Clone, Default)]
pub struct NoopLibrary {}

/// `NoopLibrary` initialization can't fail (Zig `error{}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoopInitError {}

impl std::fmt::Display for NoopInitError {
    fn fmt(&self, _f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {}
    }
}

impl std::error::Error for NoopInitError {}

impl NoopLibrary {
    pub fn init() -> Result<NoopLibrary, NoopInitError> {
        Ok(NoopLibrary {})
    }

    pub fn deinit(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_deinit() {
        let mut lib = FreetypeLibrary::init().unwrap();
        let clone = lib.clone();
        {
            let _guard = clone.mutex.lock().unwrap();
        }
        lib.deinit();

        let mut noop = NoopLibrary::init().unwrap();
        noop.deinit();
    }
}
