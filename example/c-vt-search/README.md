# Example: `ghostty-vt` Terminal Search

This contains a simple example of how to use the `ghostty-vt` search
API from C. It writes content into a terminal, searches it for a
string, navigates between the matches like a find bar, and reads the
viewport matches an embedder would use to draw highlights.

Upstream Ghostty builds this with a `build.zig`; in the Rust port,
`example/build_all.sh` compiles it with the system C compiler against
the libghostty-vt built by `tools/build_lib_vt.sh`, found with
pkg-config. Ghostty emits a standard C library that can be used with any
C tooling.

## Usage

Build and run the program (from the repository root):

```shell-session
example/build_all.sh c-vt-search
target/examples/bin/c_vt_search
```
