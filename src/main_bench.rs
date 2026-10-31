//! Port of `src/main_bench.zig`.
//!
//! STATUS: ported
//!
//! Entry point of the `ghostty-bench` benchmark runner. Zig also
//! force-references `quirks_memset.zig` here so its memset export is
//! emitted; Rust has no such export to keep alive.

pub use crate::benchmark::cli::main;
