//! The `ghostty-gen` binary (Zig: `main_gen.zig` built with
//! `-Demit-bench`). See `src/synthetic/` for the generators.

fn main() {
    if let Err(err) = ghostty::main_gen::main() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}
