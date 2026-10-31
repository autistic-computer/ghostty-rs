# Example: `ghostty-vt` SGR Parser

This contains a simple example of how to use the `ghostty-vt` SGR parser
to parse terminal styling sequences and extract text attributes.

This example demonstrates parsing a complex SGR sequence from Kakoune that
includes curly underline, RGB foreground/background colors, and RGB underline
color with mixed semicolon and colon separators.

Upstream Ghostty builds this with a `build.zig`; in the Rust port,
`example/build_all.sh` compiles it with the system C compiler against
the libghostty-vt built by `tools/build_lib_vt.sh`, found with
pkg-config. Ghostty emits a standard C library that can be used with any
C tooling.

## Usage

Build and run the program (from the repository root):

```shell-session
example/build_all.sh c-vt-sgr
target/examples/bin/c_vt_sgr
```
