# Example: `ghostty-vt` Color Scheme Report Encoding

This contains a simple example of how to use the `ghostty-vt` color scheme
report encoding API to encode terminal color scheme reports into escape
sequences.

Upstream Ghostty builds this with a `build.zig`; in the Rust port,
`example/build_all.sh` compiles it with the system C compiler against
the libghostty-vt built by `tools/build_lib_vt.sh`, found with
pkg-config. Ghostty emits a standard C library that can be used with any
C tooling.

## Usage

Build and run the program (from the repository root):

```shell-session
example/build_all.sh c-vt-color-scheme
target/examples/bin/c_vt_color_scheme
```
