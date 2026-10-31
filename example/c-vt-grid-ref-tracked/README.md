# Example: `ghostty-vt` Tracked Grid References

This contains a simple example of how to use the `ghostty-vt` terminal and
tracked grid reference APIs to keep a long-lived reference to a cell as the
terminal scrolls, detect when that reference loses its meaningful location,
and move the same tracked handle to a new point.

Upstream Ghostty builds this with a `build.zig`; in the Rust port,
`example/build_all.sh` compiles it with the system C compiler against
the libghostty-vt built by `tools/build_lib_vt.sh`, found with
pkg-config. Ghostty emits a standard C library that can be used with any
C tooling.

## Usage

Build and run the program (from the repository root):

```shell-session
example/build_all.sh c-vt-grid-ref-tracked
target/examples/bin/c_vt_grid_ref_tracked
```
