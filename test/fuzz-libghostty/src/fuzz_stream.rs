//! Port of `test/fuzz-libghostty/src/fuzz_stream.zig`.
//!
//! NOTE(port): Zig allocates the terminal from a fixed 64 MiB buffer and
//! skips inputs that exhaust it; the Rust terminal uses the global
//! allocator, and the `std.Io.Threaded` instance Zig needs for
//! `Terminal.init` has no counterpart.

use ghostty::terminal::terminal::Options;
use ghostty::terminal::Terminal;

pub fn test(input: &[u8]) {
    // Do not test zero-length input paths.
    if input.is_empty() {
        return;
    }

    // Allocate a terminal; skip the input if that fails.
    let Ok(mut t) = Terminal::init(Options {
        max_scrollback_bytes: Some(100),
        ..Options::new(80, 24)
    }) else {
        return;
    };

    let mut stream = t.vt_stream();

    // Use the first byte to decide between the scalar and slice paths
    // so both code paths get exercised by the fuzzer.
    let mode = input[0];
    let data = &input[1..];

    if mode & 1 == 0 {
        // Slice path — exercises SIMD fast-path if enabled
        stream.next_slice(data);
    } else {
        // Scalar path — exercises byte-at-a-time UTF-8 decoding
        for &byte in data {
            stream.next(byte);
        }
    }
}
