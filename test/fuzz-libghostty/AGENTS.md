# Fuzzers for Libghostty

- The fuzz targets are `src/fuzz_<name>.rs`; `fuzz_targets/<name>.rs`
  (libFuzzer) and `src/bin/fuzz-<name>.rs` (stdin) are thin front ends.
- Run a fuzzer with
  `cargo +nightly fuzz run --fuzz-dir test/fuzz-libghostty --features libfuzzer <name>`.
- `cargo test --manifest-path test/fuzz-libghostty/Cargo.toml` replays the
  committed corpora.
- Corpus directories follow the naming convention `corpus/<fuzzer>-<variant>`
  (e.g. `corpus/parser-initial`, `corpus/stream-cmin`).
- After minimizing a corpus, run `corpus/sanitize-filenames.sh`
  before committing to replace colons with underscores (colons are invalid
  on Windows NTFS).
