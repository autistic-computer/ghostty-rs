# Example: `ghostty-vt` Mode Utilities

This contains a simple example of how to use the `ghostty-vt` mode
utilities to pack and unpack terminal mode identifiers and encode
DECRPM responses.

Upstream Ghostty builds this with a `build.zig`; in the Rust port,
`example/build_all.sh` compiles it with the system C compiler against
the libghostty-vt built by `tools/build_lib_vt.sh`, found with
pkg-config. Ghostty emits a standard C library that can be used with any
C tooling.

## Usage

Build and run the program (from the repository root):

```shell-session
example/build_all.sh c-vt-modes
target/examples/bin/c_vt_modes
```
