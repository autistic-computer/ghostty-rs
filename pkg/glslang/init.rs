//! Port of `src/glslang/init.zig`.
//!
//! STATUS: ported
//!
//! glslang needs per-process initialization (`glslang_initialize_process`).
//! The naga-based implementation has no global state, so these only track
//! the init count to keep the same call protocol.

use std::sync::atomic::{AtomicUsize, Ordering};

use super::Error;

static INIT_COUNT: AtomicUsize = AtomicUsize::new(0);

pub fn init() -> Result<(), Error> {
    INIT_COUNT.fetch_add(1, Ordering::SeqCst);
    Ok(())
}

pub fn finalize() {
    let _ = INIT_COUNT.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| v.checked_sub(1));
}

/// Whether [`init`] has been called more times than [`finalize`].
pub fn is_initialized() -> bool {
    INIT_COUNT.load(Ordering::SeqCst) > 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_finalize() {
        init().unwrap();
        assert!(is_initialized());
        finalize();
    }
}
