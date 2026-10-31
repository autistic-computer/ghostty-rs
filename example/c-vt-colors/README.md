# Example: `ghostty-vt` Terminal Colors

This contains a simple example of how to set default terminal colors,
read effective and default color values, and observe how OSC overrides
layer on top of defaults using the `ghostty-vt` C library.

Upstream Ghostty builds this with a `build.zig`; in the Rust port,
`example/build_all.sh` compiles it with the system C compiler against
the libghostty-vt built by `tools/build_lib_vt.sh`, found with
pkg-config. Ghostty emits a standard C library that can be used with any
C tooling.

## Usage

Build and run the program (from the repository root):

```shell-session
example/build_all.sh c-vt-colors
target/examples/bin/c_vt_colors
```
