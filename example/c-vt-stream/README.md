# Example: VT Stream Processing in C

This contains a simple example of how to use `ghostty_terminal_vt_write`
to parse and process VT sequences in C. This is the C equivalent of
the `vt_stream` Rust example (`examples/vt_stream.rs`), ideal for read-only terminal applications
such as replay tooling, CI log viewers, and PaaS builder output.

Upstream Ghostty builds this with a `build.zig`; in the Rust port,
`example/build_all.sh` compiles it with the system C compiler against
the libghostty-vt built by `tools/build_lib_vt.sh`, found with
pkg-config. Ghostty emits a standard C library that can be used with any
C tooling.

## Usage

Build and run the program (from the repository root):

```shell-session
example/build_all.sh c-vt-stream
target/examples/bin/c_vt_stream
```
