# Example: `ghostty-vt` Selection Gestures

This contains a simple example of how to use the `ghostty-vt` selection
gesture API from C. It creates synthetic press, drag, release, and deep-press
events and formats the resulting selection snapshots.

Upstream Ghostty builds this with a `build.zig`; in the Rust port,
`example/build_all.sh` compiles it with the system C compiler against
the libghostty-vt built by `tools/build_lib_vt.sh`, found with
pkg-config. Ghostty emits a standard C library that can be used with any
C tooling.

## Usage

Build and run the program (from the repository root):

```shell-session
example/build_all.sh c-vt-selection-gesture
target/examples/bin/c_vt_selection_gesture
```
