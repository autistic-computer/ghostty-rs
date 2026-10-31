//! `fuzz-stream`: runs the stream fuzz target on one input read from stdin.

fn main() {
    ghostty_fuzz::run_stdin(ghostty_fuzz::fuzz_stream::test);
}
