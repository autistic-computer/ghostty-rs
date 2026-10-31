//! Runs every committed corpus input through its fuzz target, so corpus
//! regressions show up in `cargo test` without a fuzzing engine.

use std::fs;
use std::path::Path;

fn run_corpus(name: &str, test: fn(&[u8])) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus");
    let mut count = 0;
    for variant in ["initial", "cmin"] {
        let dir = root.join(format!("{name}-{variant}"));
        for entry in fs::read_dir(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
            let path = entry.unwrap().path();
            if path.is_file() {
                test(&fs::read(&path).unwrap());
                count += 1;
            }
        }
    }
    assert!(count > 0, "no corpus inputs for {name}");
}

#[test]
fn osc_corpus() {
    run_corpus("osc", ghostty_fuzz::fuzz_osc::test);
}

#[test]
fn parser_corpus() {
    run_corpus("parser", ghostty_fuzz::fuzz_parser::test);
}

#[test]
fn stream_corpus() {
    run_corpus("stream", ghostty_fuzz::fuzz_stream::test);
}
