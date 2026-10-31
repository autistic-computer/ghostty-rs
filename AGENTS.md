# AGENTS.md

ghrossty (repo autistic-computer/ghrossty, private) is a file-for-file Rust port of the
Ghostty terminal, currently at upstream revision 93967b7. Its main job is to be zz's terminal
engine: built with the `lib-vt` feature it exports the same `ghostty_*` C API as zz's vendored
Zig libghostty-vt, including zz's additions (screen clones, row cell copies, render state clips,
trimmed row copies, spare-page reuse). `include/` must stay identical to zz's
`third_party/ghostty/include`.

Read `PORTING.md` before changing ported code: every Zig file has a Rust counterpart at the
mirrored snake_case path, names follow the Zig ones, and tests are ported alongside.

## Commands

| Command | What it does |
|---|---|
| `cargo test --release --no-default-features --features lib-vt --lib` | The library tests CI runs on Linux, macOS and Windows |
| `cargo test --no-default-features --features lib-vt --lib terminal::` | Same with debug assertions, which turn on the slow integrity checks |
| `cargo rustc --release --no-default-features --features lib-vt --lib --crate-type staticlib` | `target/release/libghostty.a`, for C programs and comparisons against the Zig library |
| `cargo build --release --no-default-features --features app` | The app and the `ghostty-bench`/`ghostty-gen` tools (macOS needs gettext for tests, see README) |
| `tools/zig_reference.sh ZZ_CHECKOUT` | Builds zz's Zig libghostty-vt into `target/zig-reference` (needs zz's pinned Zig) |
| `test/differential/run.sh target/zig-reference` | Replays one operation log through both engines at 13 write-chunk sizes; every snapshot must match |
| `test/differential/fuzz.py target/zig-reference --time 3600` | Random operation logs (`generate.py --seed`) through both engines on half the cores; mismatches, crashes and hangs land in `target/fuzz/failures/` (`--seeds A:B`, `--jobs N`, `--checked` for a Rust build with debug assertions) |
| `test/differential/fuzz.py target/zig-reference --minimize SEED_OR_CASE` | Shrinks a failing case while it fails the same way, then prints it and the first differing value; `--replay` only runs it, `--show CASE` prints a log |
| `bench/run.sh target/zig-reference` | Times zz's hot paths through the C API on both engines (`--only`, `--runs`, `--json`); a ratio above 1 means Rust is slower |

The repo's `.cargo/config.toml` limits builds to two jobs for a small box; set
`CARGO_BUILD_JOBS` higher on bigger machines.

## Rules

- Behavior must match zz's Zig engine. Check changes to terminal state, rendering or the C API
  against it with a C program built once per library from the same source.
- Speed and memory claims come with numbers from both engines measured in the same run.
- No AI attribution in commits or PRs: no co-author trailer with an email, no model or product
  names. A co-author trailer is a funny name with no email.
- Several agents share checkouts: never stash, hard-reset or clean work you did not write.
