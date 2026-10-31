//! Port of `src/main_gen.zig`.
//!
//! STATUS: ported
//!
//! Entry point of the `ghostty-gen` synthetic data generator. Zig also
//! force-references `quirks_memset.zig` here so its memset export is
//! emitted; Rust has no such export to keep alive.

pub use crate::synthetic::cli::main;
