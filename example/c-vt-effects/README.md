# Example: `ghostty-vt` Terminal Effects

This contains a simple example of how to register and use terminal
effect callbacks (`write_pty`, `bell`, `title_changed`, and
`clipboard_write`) with the
`ghostty-vt` C library.

The clipboard effect receives one atomic write with decoded, binary-safe
MIME representations rather than protocol-specific OSC 52 data.

Upstream Ghostty builds this with a `build.zig`; in the Rust port,
`example/build_all.sh` compiles it with the system C compiler against
the libghostty-vt built by `tools/build_lib_vt.sh`, found with
pkg-config. Ghostty emits a standard C library that can be used with any
C tooling.

## Usage

Build and run the program (from the repository root):

```shell-session
example/build_all.sh c-vt-effects
target/examples/bin/c_vt_effects
```
