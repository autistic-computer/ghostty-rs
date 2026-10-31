# Example: `ghostty-vt` Render State

This contains an example of how to use the `ghostty-vt` render-state API
to create a render state, update it from terminal content, iterate rows
and cells, read styles and colors, inspect cursor and row-local selection
state, and manage dirty tracking.

Upstream Ghostty builds this with a `build.zig`; in the Rust port,
`example/build_all.sh` compiles it with the system C compiler against
the libghostty-vt built by `tools/build_lib_vt.sh`, found with
pkg-config. Ghostty emits a standard C library that can be used with any
C tooling.

## Usage

Build and run the program (from the repository root):

```shell-session
example/build_all.sh c-vt-render
target/examples/bin/c_vt_render
```
