# Examples

Standalone programs demonstrating the libghostty-vt C API, copied from
upstream Ghostty's `example/` directory. The directories starting with `c-`
use the C API from C and `cpp-` from C++. The Zig API examples (`zig-vt`,
`zig-vt-stream`, `zig-formatter`) are ported as Rust examples of the
crate's API in `examples/` (`cargo run --example vt`, `vt_stream`,
`formatter`); the Swift and Wasm examples are not included.

Upstream builds each example with its own `build.zig`. Here,
`build_all.sh` builds libghostty-vt with `tools/build_lib_vt.sh`, compiles
every example with the system C/C++ compiler against it using pkg-config
(`c-vt-static` and `c-vt-snapshot` link `libghostty-vt-static`, the others
the shared library), builds the `c-vt-cmake*` examples with CMake, and runs
them all:

```shell-session
example/build_all.sh                 # all examples
example/build_all.sh c-vt-colors     # just one
target/examples/bin/c_vt_colors      # run it again
```

Each program's output is saved in `target/examples/<example>.out` (and
`.err`). `--prefix <dir>` uses another installed libghostty-vt instead, e.g.
the prefix of `zig build -Demit-lib-vt`, so the outputs of the Rust and Zig
libraries can be compared with `diff -r`.
