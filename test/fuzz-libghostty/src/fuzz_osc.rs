//! Port of `test/fuzz-libghostty/src/fuzz_osc.zig`.
//!
//! NOTE(port): Zig backs the parser's allocator with a fixed 8 MiB buffer
//! (`mem.zig`'s `FuzzAllocator`) that is reset for every input; the Rust
//! parser allocates from the global allocator (`osc::Allocator::GLOBAL`),
//! so `mem.zig` has no counterpart.

use ghostty::terminal::osc;

pub fn test(input: &[u8]) {
    // Need at least one byte for the terminator selector.
    if input.is_empty() {
        return;
    }

    // Use the first byte to select the terminator variant.
    let selector = input[0];
    let payload = &input[1..];

    let mut p = osc::Parser::init(Some(osc::Allocator::GLOBAL));
    for &byte in payload {
        p.next(byte);
    }

    // Exercise all three terminator paths:
    //   0 -> BEL  (0x07)
    //   1 -> ST   (0x9c)
    //   2 -> missing terminator (null)
    let terminator = match selector % 3 {
        0 => Some(0x07),
        1 => Some(0x9c),
        _ => None,
    };

    let _ = p.end(terminator);
}
