//! The `ghostty-bench` binary (Zig: `main_bench.zig` built with
//! `-Demit-bench`). See `src/benchmark/` for the benchmarks.

fn main() {
    if let Err(err) = ghostty::main_bench::main() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}
