//! Port of `test/fuzz-libghostty/src/fuzz_parser.zig`.

use ghostty::terminal::Parser;

pub fn test(input: &[u8]) {
    let mut p = Parser::init();
    for &byte in input {
        let _ = p.next(byte);
    }
}
