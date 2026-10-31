# Fuzzers for Libghostty

This directory contains fuzzing harnesses for libghostty-vt, ported from
Ghostty's Zig [AFL++](https://aflplus.plus/) harnesses.

## Fuzz Targets

| Target   | Binary        | Description                                             |
| -------- | ------------- | ------------------------------------------------------- |
| `osc`    | `fuzz-osc`    | OSC parser with allocator (`osc.Parser.next` + `end`)   |
| `parser` | `fuzz-parser` | VT parser only (`Parser.next` byte-at-a-time)           |
| `stream` | `fuzz-stream` | Full terminal stream (`nextSlice` + `next` via handler) |

The osc target directly fuzzes the `osc.Parser` with an allocator enabled,
exercising the allocating writer code paths for large payloads. The first
byte selects the terminator variant (BEL, ST, or missing). Seeds cover OSC
52, 66, 133, 3008, 1337, and 5522.

The stream target creates a small `Terminal` and exercises the readonly
`Stream` handler, covering printing, CSI dispatch, OSC, DCS, SGR, cursor
movement, scrolling regions, and more. The first byte of each input selects
between the slice path (SIMD fast-path) and the scalar path.

## Prerequisites

The Rust port runs the same targets under libFuzzer via
[cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) (nightly Rust):

```sh
cargo install cargo-fuzz
rustup toolchain install nightly
```

AFL++ can still drive the plain stdin binaries (`fuzz-osc`, `fuzz-parser`,
`fuzz-stream`), e.g. in QEMU mode, but they are not instrumented.

## Building

From the repository root:

```sh
cargo build --release --manifest-path test/fuzz-libghostty/Cargo.toml
```

This builds the stdin harnesses at `target/release/fuzz-{osc,parser,stream}`
(in `test/fuzz-libghostty/target` unless `CARGO_TARGET_DIR` is set).
`cargo test --manifest-path test/fuzz-libghostty/Cargo.toml` replays every
committed corpus input through its target.

## Running the Fuzzer

```sh
cargo +nightly fuzz run --fuzz-dir test/fuzz-libghostty --features libfuzzer osc
cargo +nightly fuzz run --fuzz-dir test/fuzz-libghostty --features libfuzzer parser
cargo +nightly fuzz run --fuzz-dir test/fuzz-libghostty --features libfuzzer stream
```

Pass a corpus directory after the target name to seed it (copy a
`corpus/<target>-*` directory first; libFuzzer adds new inputs to it), and
libFuzzer options after `--`, e.g. `-- -max_total_time=600`.

The fuzzer runs indefinitely. Let it run for as long as you like; meaningful
coverage is usually reached within a few hours, but longer runs can find
deeper bugs. Press `ctrl+c` to stop the fuzzer when you're done.

## Finding and Reproducing Crashes

libFuzzer writes crashing inputs to `test/fuzz-libghostty/artifacts/<target>/`.
Replay one with libFuzzer or by piping it into the stdin harness:

```sh
cargo +nightly fuzz run --fuzz-dir test/fuzz-libghostty --features libfuzzer stream artifacts/stream/<file>
target/release/fuzz-stream < artifacts/stream/<file>
```

## Corpus Management

libFuzzer adds new interesting inputs to the corpus directory it runs on.
Minimize it with libFuzzer's merge mode before committing:

```sh
cargo +nightly fuzz cmin --fuzz-dir test/fuzz-libghostty --features libfuzzer stream corpus-dir
```

The committed `*-cmin` corpora came from `afl-cmin` runs of the Zig
harnesses; the targets read the same input format, so they remain valid
seeds.

### Windows compatibility

AFL++ output filenames contain colons (e.g., `id:000024,time:0,...`), which
are invalid on Windows (NTFS). After running `afl-cmin`,
rename the output files to replace colons with underscores before committing:

```sh
./corpus/sanitize-filenames.sh
```

### Corpus directories

| Directory                | Contents                                        |
| ------------------------ | ----------------------------------------------- |
| `corpus/osc-initial/`    | Hand-written seed inputs for osc-parser         |
| `corpus/osc-cmin/`       | Output of `afl-cmin` (edge-deduplicated corpus) |
| `corpus/parser-initial/` | Hand-written seed inputs for vt-parser          |
| `corpus/parser-cmin/`    | Output of `afl-cmin` (edge-deduplicated corpus) |
| `corpus/stream-initial/` | Hand-written seed inputs for vt-stream          |
| `corpus/stream-cmin/`    | Output of `afl-cmin` (edge-deduplicated corpus) |
