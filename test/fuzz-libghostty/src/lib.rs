//! Fuzz targets for libghostty-vt (Zig: `test/fuzz-libghostty/src/`).
//!
//! Each module exposes `test(input)`, the body of Zig's `zig_fuzz_test`.

#[path = "fuzz_osc.rs"]
pub mod fuzz_osc;
#[path = "fuzz_parser.rs"]
pub mod fuzz_parser;
#[path = "fuzz_stream.rs"]
pub mod fuzz_stream;

/// Read a whole fuzz input from stdin and run `test` on it (the stdin
/// harness of the Zig build's `afl.c`).
pub fn run_stdin(test: fn(&[u8])) {
    use std::io::Read;
    let mut input = Vec::new();
    std::io::stdin()
        .read_to_end(&mut input)
        .expect("failed to read fuzz input from stdin");
    test(&input);
}
