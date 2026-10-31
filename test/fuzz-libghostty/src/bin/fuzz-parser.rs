//! `fuzz-parser`: runs the parser fuzz target on one input read from stdin.

fn main() {
    ghostty_fuzz::run_stdin(ghostty_fuzz::fuzz_parser::test);
}
