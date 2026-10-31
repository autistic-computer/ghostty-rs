//! Port of `src/main.zig`.
//!
//! STATUS: ported
//!
//! The binary entry point. Zig picks the entrypoint module at comptime from
//! `build_config.exe_entrypoint`; the Rust `ghostty` binary is always the
//! `ghostty` entrypoint (the generator tools are separate binaries).
//! `quirks_memset` (a Zig toolchain workaround) has no Rust counterpart.

/// The main entrypoint for the program.
fn main() {
    if let Err(err) = ghostty::main_ghostty::main() {
        log::error!("{err}");
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}
